//! Binary output capture. Workers own only OS pipes and Rust buffers.

use std::io::{self, Read};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

type Drain = fn(usize, &mut dyn Read) -> io::Result<Vec<u8>>;

struct Running<'a> {
    child: &'a mut Child,
    readers: Vec<JoinHandle<()>>,
    reaped: bool,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if !self.reaped {
            // Stop the direct child before joining readers blocked on its pipes.
            // Process-tree cancellation is outside this synchronous boundary.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn read_all(_: usize, pipe: &mut dyn Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn reader(
    mut pipe: impl Read + Send + 'static,
    index: usize,
    completed: Sender<(usize, io::Result<Vec<u8>>)>,
    drain: Drain,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().spawn(move || {
        // Report failures immediately: waiting for the other stream first can
        // deadlock when the child is blocked writing to the failed stream.
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drain(index, &mut pipe)))
                .unwrap_or_else(|_| Err(io::Error::other("process output reader panicked")));
        let _ = completed.send((index, result));
    })
}

fn capture_started(child: &mut Child, drain: Drain) -> io::Result<Output> {
    let mut running = Running {
        child,
        readers: Vec::new(),
        reaped: false,
    };
    let stdout = running
        .child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing process stdout pipe"))?;
    let stderr = running
        .child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing process stderr pipe"))?;
    let (completed, results) = mpsc::channel();
    running
        .readers
        .push(reader(stdout, 0, completed.clone(), drain)?);
    running.readers.push(reader(stderr, 1, completed, drain)?);
    let mut streams = [Vec::new(), Vec::new()];
    for _ in 0..2 {
        let (index, result) = results
            .recv()
            .map_err(|_| io::Error::other("process output reader disconnected"))?;
        streams[index] = result?;
    }
    let status = running.child.wait()?;
    running.reaped = true;
    let [stdout, stderr] = streams;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

pub(super) fn capture(mut command: Command) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    capture_started(&mut child, read_all)
}

#[cfg(test)]
pub(super) const TEST_CHILD: &str = "LOOM_RUNTIME_CAPTURE_CHILD";
#[cfg(test)]
pub(super) const TEST_NAME: &str = "process_io::tests::capture_child_fixture";
#[cfg(test)]
pub(super) const TEST_TOKEN: &str = "literal argument 雪🙂 *? \" '$;";
#[cfg(test)]
pub(super) const TEST_BYTES: usize = 256 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    #[ignore = "child fixture for process capture tests"]
    fn capture_child_fixture() {
        if std::env::var_os(TEST_CHILD).as_deref() == Some(std::ffi::OsStr::new("blocked")) {
            loop {
                thread::park();
            }
        }
        if !std::env::args().any(|argument| argument == TEST_TOKEN) {
            return;
        }
        assert_eq!(io::stdin().read(&mut [0]).unwrap(), 0);
        let stdout = (0..TEST_BYTES).map(|index| index as u8).collect::<Vec<_>>();
        let stderr = stdout.iter().map(|byte| 255 - byte).collect::<Vec<_>>();
        io::stdout().write_all(&stdout).unwrap();
        io::stdout().flush().unwrap();
        io::stderr().write_all(&stderr).unwrap();
        io::stderr().flush().unwrap();
        std::process::exit(7);
    }

    #[test]
    fn setup_and_read_failures_reap_the_child() {
        fn fail_read(index: usize, pipe: &mut dyn Read) -> io::Result<Vec<u8>> {
            if index == 0 {
                Err(io::Error::other("injected read failure"))
            } else {
                read_all(index, pipe)
            }
        }
        for missing_pipe in [false, true] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", TEST_NAME, "--nocapture"])
                .env(TEST_CHILD, "blocked")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            if missing_pipe {
                drop(child.stdout.take());
            }
            let error = capture_started(&mut child, fail_read).unwrap_err();
            assert_eq!(
                error.to_string(),
                if missing_pipe {
                    "missing process stdout pipe"
                } else {
                    "injected read failure"
                }
            );
            assert!(child.try_wait().unwrap().is_some());
        }
    }
}
