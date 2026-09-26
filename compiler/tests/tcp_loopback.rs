use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpStream},
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};
mod common;
use common::success;

const PACKAGE: &str = "compiler/examples/tcp_loopback";

struct Server(Option<Child>);

impl Server {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().unwrap()
    }

    fn finish(mut self) -> Output {
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn loopback(command: &mut Command, stress: bool) {
    if stress {
        command.env("LOOM_GC_STRESS", "1");
    } else {
        command.env_remove("LOOM_GC_STRESS");
    }
    let mut server = Server(Some(
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let stdout = server.child().stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        let mut first = String::new();
        let result = stdout.read_line(&mut first).map(|_| first);
        let _ = sender.send(result);
        let mut rest = Vec::new();
        stdout.read_to_end(&mut rest).unwrap();
        rest
    });
    let first = match receiver.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(line)) => line,
        other => {
            panic!("TCP server did not publish its port: {other:?}");
        }
    };
    let port: u16 = first.trim().parse().unwrap();
    let mut peer = TcpStream::connect(("127.0.0.1", port)).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let payload: Vec<u8> = (0..70001).map(|index| (index % 256) as u8).collect();
    peer.write_all(&payload).unwrap();
    peer.shutdown(Shutdown::Write).unwrap();
    let mut echoed = Vec::new();
    peer.read_to_end(&mut echoed).unwrap();
    assert_eq!(echoed, payload);
    let output = server.finish();
    let rest = reader.join().unwrap();
    success(&output);
    assert!(
        rest.is_empty(),
        "unexpected stdout: {}",
        String::from_utf8_lossy(&rest)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn source_tcp_check_build_test_run_under_forced_gc() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "tcp-loopback");
    let ir = directory.path().join("tcp-loopback.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&["check", PACKAGE])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &common::command(&["test", PACKAGE])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &Command::new(common::executable(
                &common::root().join(PACKAGE).join("target"),
                "tests",
            ))
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
        );
        success(
            &common::command(&[
                "build",
                PACKAGE,
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        loopback(&mut Command::new(&executable), true);
        loopback(
            common::command(&["run", PACKAGE]).env("LOOM_OPT_LEVEL", level),
            false,
        );
        let llvm = std::fs::read_to_string(&ir).unwrap();
        assert!(llvm.contains("loom_rt_task_wait_socket"));
        assert!(llvm.contains("loom_rt_socket_read"));
        assert!(llvm.contains("loom_rt_socket_write_bytes"));
    }
}
