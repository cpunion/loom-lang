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
                "suspended cleanup failed to drain: {:?}",
                child.wait_with_output()
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn moving_captures_and_scoped_resources_survive_suspension_and_lexical_exits() {
    let package = "compiler/examples/async_cleanup";
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "cleanup");
    let ir = temporary.path().join("cleanup.ll");
    success(&common::loom(&["check", package]));
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package,
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
        assert_eq!(output.stdout, b"cleanup finished\n");
        assert!(output.stderr.is_empty());
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(
            ir.contains("loom_rt_task_cleanup_push") && ir.contains("loom_rt_task_cleanup_pop")
        );
        assert!(!ir.contains("llvm.coro") && !ir.contains("universal"));
        success(
            &common::command(&["test", package])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        let tests = common::executable(&common::root().join(package).join("target"), "tests");
        success(&run(&tests));
    }
}

#[test]
fn faults_cancel_waits_before_children_helpers_and_parent_cleanup() {
    let package = tempfile::tempdir().unwrap();
    let source = r#"
import std.time.sleep_ms
import std.text.concat
import std.io.write_text

async fn child() {
    var message = "stale child"
    defer { discard write_text(concat(message, "\n")) }
    defer {
        message = concat("child ", "cleaned")
        assert false
    }
    sleep_ms(600000).await
    discard write_text("cancelled child ran\n")
}
async fn unstarted() {
    defer { discard write_text("unstarted cleanup ran\n") }
}
fn fail() {
    defer { discard write_text("helper cleaned\n") }
    assert false
}
async fn main() {
    var message = "stale parent"
    defer { discard write_text(concat(message, "\n")) }
    let waiting = child()
    sleep_ms(1).await
    message = concat("parent ", "cleaned")
    let queued = unstarted()
    fail()
    waiting.await
    queued.await
}
"#;
    fs::write(package.path().join("main.loom"), source).unwrap();
    let executable = common::executable(package.path(), "cancel-cleanup");
    let assertion_line = source
        .lines()
        .position(|line| line == "fn fail() {")
        .unwrap()
        + 3;
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package.path().to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = run(&executable);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            output.stdout,
            b"child cleaned\nhelper cleaned\nparent cleaned\n"
        );
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostic.contains(&format!("main.loom:{assertion_line}:")),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("assertion failed"), "{diagnostic}");
        assert_eq!(diagnostic.matches("RuntimeFault:").count(), 1);
    }
}
