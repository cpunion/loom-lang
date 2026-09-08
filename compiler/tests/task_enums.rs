use std::fs;
mod common;
use common::{run_tasks, success};

#[test]
fn enum_tasks_match_transfer_and_propagate_natively() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "enums");
    success(&common::loom(&["check", "compiler/examples/task_enums"]));
    success(&common::loom(&["test", "compiler/examples/task_enums"]));
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                "compiler/examples/task_enums",
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = run_tasks(&executable);
        success(&output);
        assert_eq!(output.stdout, b"enums finished\n");
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn enum_adoption_cancels_active_payload_before_receiver_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.loom"),
        r#"
import std.time.sleep_ms
import std.io.write_text
enum Work { Empty Pair(Task[Int], Task[Int]) }
async fn child(name Text) Int {
    defer { discard write_text(name) }
    sleep_ms(600000).await
    1
}
async fn produce() Work {
    Work.Pair(child("left cleaned\n"), child("right cleaned\n"))
}
async fn fail(value Work) Int {
    defer { discard write_text("receiver cleaned\n") }
    sleep_ms(1).await
    assert false
    match value { Work.Pair(a, b) => a.await + b.await, Work.Empty => 0 }
}
async fn main() {
    defer { discard write_text("main cleaned\n") }
    let pending = produce()
    sleep_ms(1).await
    let value = pending.await
    discard fail(value).await
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
        let output = run_tasks(&executable);
        assert_eq!(output.status.code(), Some(1));
        let text = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        let mut children = lines[..2].to_vec();
        children.sort();
        assert_eq!(children, ["left cleaned", "right cleaned"]);
        assert_eq!(&lines[2..], ["receiver cleaned", "main cleaned"]);
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("assertion failed"), "{diagnostic}");
        assert!(
            diagnostic.contains("main.loom:24:13: task created here"),
            "{diagnostic}"
        );
        assert_eq!(diagnostic.matches("RuntimeFault:").count(), 1);
    }
}
