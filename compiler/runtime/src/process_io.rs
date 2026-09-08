//! Binary input and output capture. Workers own only OS pipes and Rust buffers.

use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

type Drain = fn(usize, &mut dyn Read) -> io::Result<Vec<u8>>;

pub(super) fn valid_env_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['=', '\0'])
}

pub(super) fn configure<'a>(
    mut command: Command,
    directory: &str,
    clear: i64,
    changes: impl Iterator<Item = &'a str>,
) -> Result<Command, i64> {
    if directory.contains('\0') || !matches!(clear, 0 | 1) {
        return Err(-4);
    }
    if !directory.is_empty() {
        command.current_dir(directory);
    }
    if clear == 1 {
        command.env_clear();
    }
    for change in changes {
        if change.contains('\0') {
            return Err(-4);
        }
        if let Some((name, value)) = change.split_once('=') {
            if !valid_env_name(name) {
                return Err(-4);
            }
            command.env(name, value);
        } else {
            if !valid_env_name(change) {
                return Err(-4);
            }
            command.env_remove(change);
        }
    }
    Ok(command)
}

struct Running<'a> {
    child: &'a mut Child,
    workers: Vec<JoinHandle<()>>,
    reaped: bool,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if !self.reaped {
            // Stop the direct child before joining workers blocked on its pipes.
            // Process-tree cancellation is outside this synchronous boundary.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn read_all(_: usize, pipe: &mut dyn Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

enum Completion {
    Output(usize, io::Result<Vec<u8>>),
    Input(io::Result<()>),
}

fn reader(
    mut pipe: impl Read + Send + 'static,
    index: usize,
    completed: Sender<Completion>,
    drain: Drain,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().spawn(move || {
        // Report failures immediately: waiting for the other stream first can
        // deadlock when the child is blocked writing to the failed stream.
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drain(index, &mut pipe)))
                .unwrap_or_else(|_| Err(io::Error::other("process output reader panicked")));
        let _ = completed.send(Completion::Output(index, result));
    })
}

fn writer(
    mut pipe: ChildStdin,
    input: Vec<u8>,
    completed: Sender<Completion>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            block_sigpipe(&pipe)?;
            let result = pipe.write_all(&input);
            if result
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
            {
                consume_sigpipe()?;
            }
            result
        }))
        .unwrap_or_else(|_| Err(io::Error::other("process input writer panicked")));
        // Closing stdin signals EOF even when the child consumes all input before
        // producing output. A child may intentionally stop reading early; keep
        // its output and exit status in that case.
        drop(pipe);
        let result = match result {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            other => other,
        };
        let _ = completed.send(Completion::Input(result));
    })
}

fn block_sigpipe(_pipe: &ChildStdin) -> io::Result<()> {
    #[cfg(any(target_vendor = "apple", target_os = "netbsd"))]
    {
        use std::os::fd::AsRawFd;
        // Darwin sends pipe-write SIGPIPE process-wide, so a thread mask alone
        // is insufficient. Suppress it on this owned pipe without changing any
        // global signal disposition. Darwin's SDK defines F_SETNOSIGPIPE as 73;
        // libc currently exports the constant only for NetBSD.
        #[cfg(target_vendor = "apple")]
        const NO_SIGPIPE: libc::c_int = 73;
        #[cfg(target_os = "netbsd")]
        const NO_SIGPIPE: libc::c_int = libc::F_SETNOSIGPIPE;
        // SAFETY: The live ChildStdin owns this writable pipe descriptor.
        if unsafe { libc::fcntl(_pipe.as_raw_fd(), NO_SIGPIPE, 1) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(all(unix, not(any(target_vendor = "apple", target_os = "netbsd"))))]
    {
        // Native Loom does not run Rust's main initialization, which ignores
        // SIGPIPE. Block it only on this dedicated writer thread; never change
        // the caller's or child's signal disposition.
        // SAFETY: All pointers reference live sigset_t storage and pthread_sigmask
        // changes only this thread's signal mask.
        unsafe {
            let mut signals = std::mem::zeroed::<libc::sigset_t>();
            if libc::sigemptyset(&mut signals) != 0
                || libc::sigaddset(&mut signals, libc::SIGPIPE) != 0
            {
                return Err(io::Error::last_os_error());
            }
            let error = libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut());
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
        }
    }
    Ok(())
}

fn consume_sigpipe() -> io::Result<()> {
    #[cfg(all(unix, not(any(target_vendor = "apple", target_os = "netbsd"))))]
    {
        // Consume a signal raised by our write before the thread exits. If the
        // process already ignores SIGPIPE, no signal is pending and sigwait would
        // block forever. This dedicated thread never performs any other writes.
        // SAFETY: All pointers reference live signal-set or integer storage;
        // SIGPIPE is blocked for this thread before its write begins.
        unsafe {
            let mut pending = std::mem::zeroed::<libc::sigset_t>();
            if libc::sigpending(&mut pending) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::sigismember(&pending, libc::SIGPIPE) == 1 {
                let mut signals = std::mem::zeroed::<libc::sigset_t>();
                libc::sigemptyset(&mut signals);
                libc::sigaddset(&mut signals, libc::SIGPIPE);
                let mut signal = 0;
                let error = libc::sigwait(&signals, &mut signal);
                if error != 0 {
                    return Err(io::Error::from_raw_os_error(error));
                }
            }
        }
    }
    Ok(())
}

fn capture_started(child: &mut Child, input: Vec<u8>, drain: Drain) -> io::Result<Output> {
    let mut running = Running {
        child,
        workers: Vec::new(),
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
        .workers
        .push(reader(stdout, 0, completed.clone(), drain)?);
    running
        .workers
        .push(reader(stderr, 1, completed.clone(), drain)?);
    if !input.is_empty() {
        let stdin = running
            .child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing process stdin pipe"))?;
        running
            .workers
            .push(writer(stdin, input, completed.clone())?);
    } else {
        drop(running.child.stdin.take());
    }
    drop(completed);
    let mut streams = [Vec::new(), Vec::new()];
    for _ in 0..running.workers.len() {
        let completed = results
            .recv()
            .map_err(|_| io::Error::other("process I/O worker disconnected"))?;
        match completed {
            Completion::Output(index, result) => streams[index] = result?,
            Completion::Input(result) => result?,
        }
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

pub(super) fn capture(mut command: Command, input: Vec<u8>) -> io::Result<Output> {
    let mut child = command
        .stdin(if input.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    capture_started(&mut child, input, read_all)
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

    #[test]
    fn configuration_is_child_local_ordered_and_validated() {
        let mut original = Command::new("unused");
        original.env("OLD", "discarded");
        let command = configure(
            original,
            "working directory 雪",
            1,
            [
                "VALUE=first",
                "VALUE",
                "VALUE=last=part",
                "EMPTY=",
                "REMOVED",
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("working directory 雪"))
        );
        let values = command
            .get_envs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert!(!values.contains_key(std::ffi::OsStr::new("OLD")));
        assert_eq!(
            values[std::ffi::OsStr::new("VALUE")],
            Some(std::ffi::OsStr::new("last=part"))
        );
        assert_eq!(
            values[std::ffi::OsStr::new("EMPTY")],
            Some(std::ffi::OsStr::new(""))
        );
        let removed = configure(Command::new("unused"), "", 0, ["REMOVED"].into_iter()).unwrap();
        assert_eq!(
            removed.get_envs().collect::<Vec<_>>(),
            [(std::ffi::OsStr::new("REMOVED"), None)]
        );
        for (directory, clear, change) in [
            ("bad\0dir", 0, "OK=yes"),
            ("", 2, "OK=yes"),
            ("", 0, ""),
            ("", 0, "=value"),
            ("", 0, "NAME=bad\0value"),
        ] {
            assert!(matches!(
                configure(
                    Command::new("unused"),
                    directory,
                    clear,
                    [change].into_iter()
                ),
                Err(-4)
            ));
        }
        #[cfg(windows)]
        {
            let command = configure(
                Command::new("unused"),
                "",
                1,
                ["Name=first", "NAME=last"].into_iter(),
            )
            .unwrap();
            let values = command.get_envs().collect::<Vec<_>>();
            assert_eq!(values.len(), 1);
            assert_eq!(values[0].1, Some(std::ffi::OsStr::new("last")));
        }
    }

    #[test]
    #[ignore = "child fixture for process capture tests"]
    fn capture_child_fixture() {
        #[cfg(unix)]
        if std::env::var_os(TEST_CHILD).as_deref() == Some(std::ffi::OsStr::new("sigpipe")) {
            // SAFETY: This isolated subprocess models a native Loom executable;
            // the test runner's process-wide signal disposition is untouched.
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            }
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--ignored",
                    "--exact",
                    TEST_NAME,
                    "--nocapture",
                    "--skip",
                    TEST_TOKEN,
                ])
                .env(TEST_CHILD, "early");
            let output = capture(command, vec![0; TEST_BYTES]).unwrap();
            assert_eq!(output.status.code(), Some(7));
            assert!(output.stdout.len() >= TEST_BYTES);
            assert_eq!(output.stderr.len(), TEST_BYTES);
            std::process::exit(0);
        }
        if std::env::var_os(TEST_CHILD).as_deref() == Some(std::ffi::OsStr::new("blocked")) {
            loop {
                thread::park();
            }
        }
        if !std::env::args().any(|argument| argument == TEST_TOKEN) {
            return;
        }
        let mode = std::env::var(TEST_CHILD).unwrap_or_default();
        if mode.is_empty() {
            assert_eq!(io::stdin().read(&mut [0]).unwrap(), 0);
        }
        let stdout = (0..TEST_BYTES).map(|index| index as u8).collect::<Vec<_>>();
        let stderr = stdout.iter().map(|byte| 255 - byte).collect::<Vec<_>>();
        io::stdout().write_all(&stdout).unwrap();
        io::stdout().flush().unwrap();
        io::stderr().write_all(&stderr).unwrap();
        io::stderr().flush().unwrap();
        if mode == "input" {
            let mut input = Vec::new();
            io::stdin().read_to_end(&mut input).unwrap();
            assert_eq!(input, stdout);
            io::stdout().write_all(&input).unwrap();
            io::stdout().flush().unwrap();
        }
        std::process::exit(7);
    }

    #[test]
    fn binary_input_is_written_while_both_outputs_are_drained() {
        let input = (0..TEST_BYTES).map(|index| index as u8).collect::<Vec<_>>();
        for mode in ["input", "early", ""] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--ignored",
                    "--exact",
                    TEST_NAME,
                    "--nocapture",
                    "--skip",
                    TEST_TOKEN,
                ])
                .env(TEST_CHILD, mode);
            let output = capture(
                command,
                if mode.is_empty() {
                    Vec::new()
                } else {
                    input.clone()
                },
            )
            .unwrap();
            assert_eq!(output.status.code(), Some(7));
            let expected = if mode == "input" {
                input.repeat(2)
            } else {
                input.clone()
            };
            assert!(output.stdout.ends_with(&expected));
            assert_eq!(
                output.stderr,
                input.iter().map(|byte| 255 - byte).collect::<Vec<_>>()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn early_stdin_close_preserves_output_with_default_sigpipe() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", TEST_NAME, "--nocapture"])
            .env(TEST_CHILD, "sigpipe")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
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
        for missing_pipe in [None, Some("stdout"), Some("stdin")] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", TEST_NAME, "--nocapture"])
                .env(TEST_CHILD, "blocked")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            if missing_pipe == Some("stdout") {
                drop(child.stdout.take());
            }
            if missing_pipe == Some("stdin") {
                drop(child.stdin.take());
            }
            let error = capture_started(&mut child, vec![0; TEST_BYTES], fail_read).unwrap_err();
            assert_eq!(
                error.to_string(),
                match missing_pipe {
                    Some("stdout") => "missing process stdout pipe",
                    Some("stdin") => "missing process stdin pipe",
                    _ => "injected read failure",
                }
            );
            assert!(child.try_wait().unwrap().is_some());
        }
    }
}
