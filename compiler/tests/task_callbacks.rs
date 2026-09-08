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
                "callback tasks failed to drain: {:?}",
                child.wait_with_output()
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn task_callbacks_use_native_pointers_through_storage_and_suspension() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "callbacks");
    success(&common::loom(&[
        "check",
        "compiler/examples/task_callbacks",
    ]));
    success(&common::loom(&["test", "compiler/examples/task_callbacks"]));
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                "compiler/examples/task_callbacks",
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = run(&executable);
        success(&output);
        assert_eq!(output.stdout, b"callbacks finished\n");
        assert!(output.stderr.is_empty());
    }

    // No async function is reachable, but exported callable signatures still
    // use the same ABI. Ordinary direct Task forwarding gains no scheduler.
    fs::write(directory.path().join("main.loom"),
        "pub fn identity(callback fn(Int) Task[Int]) fn(Int) Task[Int] { callback }\nfn pass(task Task[Int]) Task[Int] { task }\npub fn select() fn(Task[Int]) Task[Int] { pass }").unwrap();
    let ir = directory.path().join("library.ll");
    success(&common::loom(&[
        "build",
        directory.path().to_str().unwrap(),
        "--output",
        directory.path().join("library.o").to_str().unwrap(),
        "--emit-ir",
        ir.to_str().unwrap(),
    ]));
    let text = fs::read_to_string(ir).unwrap();
    assert!(!text.contains("loom_rt_task_create") && !text.contains("loom_rt_task_run"));
}

#[test]
fn indirect_creation_blame_and_adopted_child_cleanup_survive_faults() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.loom"),
        r#"
import std.time.sleep_ms
import std.io.write_text
async fn child() Int {
    defer { discard write_text("child cleaned\n") }
    sleep_ms(600000).await
    1
}
async fn fail(task Task[Int]) Int {
    defer { discard write_text("callback cleaned\n") }
    sleep_ms(1).await
    assert false
    task.await
}
fn select() fn(Task[Int]) Task[Int] { fail }
async fn main() {
    let callback = select()
    discard callback(child()).await
}
"#,
    )
    .unwrap();
    let executable = common::executable(directory.path(), "fault");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                directory.path().to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = run(&executable);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"child cleaned\ncallback cleaned\n");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("assertion failed"), "{diagnostic}");
        assert!(
            diagnostic.contains("main.loom:18:13: task created here"),
            "{diagnostic}"
        );
        assert_eq!(diagnostic.matches("RuntimeFault:").count(), 1);
    }
}
