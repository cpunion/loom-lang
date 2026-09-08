use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
mod common;
use common::success;

fn run(executable: &Path) -> Output {
    let mut child = Command::new(executable)
        .env("LOOM_GC_STRESS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let limit = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= limit {
            child.kill().unwrap();
            panic!(
                "method tasks failed to drain: {:?}",
                child.wait_with_output()
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn static_and_dynamic_methods_use_typed_constructors_and_sparse_witnesses() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "methods");
    let ir = directory.path().join("methods.ll");
    success(&common::loom(&["check", "compiler/examples/async_methods"]));
    success(&common::loom(&["test", "compiler/examples/async_methods"]));
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                "compiler/examples/async_methods",
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = run(&executable);
        success(&output);
        assert_eq!(output.stdout, b"methods finished\n");
        assert!(output.stderr.is_empty());
        let text = fs::read_to_string(&ir).unwrap();
        assert!(text.contains("loom.witness") && text.contains("loom_rt_task_create"));
        assert!(!text.contains("UNUSED_ASYNC_METHOD"));
    }

    // An exported interface still lowers its async ABI even when this library
    // has no concrete async functions or witnesses to activate.
    fs::write(
        directory.path().join("main.loom"),
        "pub concept Reader { async fn read(self Self) Int }\npub fn identity(value dyn Reader) dyn Reader { value }",
    )
    .unwrap();
    success(&common::loom(&[
        "build",
        directory.path().to_str().unwrap(),
        "--output",
        directory.path().join("library.o").to_str().unwrap(),
    ]));
}

#[test]
fn dynamic_method_faults_drain_adopted_children_and_keep_creation_blame() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.loom"),
        r#"
import std.time.sleep_ms
import std.io.write_text

concept Runner { async fn run(self Self, task Task[Int]) Int }
record Worker {}
impl Runner for Worker {
    async fn run(self Worker, task Task[Int]) Int {
        defer { discard write_text("method cleaned\n") }
        sleep_ms(1).await
        assert false
        task.await
    }
}
async fn child() Int {
    defer { discard write_text("child cleaned\n") }
    sleep_ms(600000).await
    discard write_text("cancelled child resumed\n")
    1
}
async fn main() {
    let worker dyn Runner = Worker {}
    discard worker.run(child()).await
}
"#,
    )
    .unwrap();
    let executable = common::executable(directory.path(), "fault");
    success(&common::loom(&[
        "build",
        directory.path().to_str().unwrap(),
        "--output",
        executable.to_str().unwrap(),
    ]));
    let output = run(&executable);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"child cleaned\nmethod cleaned\n");
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("assertion failed"), "{diagnostic}");
    assert!(
        diagnostic.contains("main.loom:23:13: task created here"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("task created here"), "{diagnostic}");
    assert_eq!(diagnostic.matches("RuntimeFault:").count(), 1);
}
