use std::fs;
mod common;
use common::{run_tasks, success};

#[test]
fn task_lists_transfer_and_drain_native_recursive_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "lists");
    success(&common::loom(&["check", "compiler/examples/task_lists"]));
    success(&common::loom(&["test", "compiler/examples/task_lists"]));
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                "compiler/examples/task_lists",
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = run_tasks(&executable);
        success(&output);
        assert_eq!(output.stdout, b"lists finished\n");
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn task_list_adoption_drains_every_child_before_parent_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.loom"),
        r#"
import std.list.transfer.append
import std.list.transfer.take_last
import std.option.Option
import std.time.sleep_ms
import std.io.write_text
async fn child() List[Text] {
    defer { discard write_text("child cleaned\n") }
    sleep_ms(600000).await
    ["payload"]
}
async fn produce() List[Task[List[Text]]] {
    var pending List[Task[List[Text]]] = []
    var count = 0
    while count < 8 {
        pending = append(pending, child())
        count = count + 1
    }
    pending
}
async fn fail(values List[Task[List[Text]]]) {
    defer { discard write_text("receiver cleaned\n") }
    sleep_ms(1).await
    assert false
    var pending = values
    while true {
        match take_last(pending) {
            Option.Some(pair) => {
                let task, rest = pair
                pending = rest
                discard task.await
            }
            Option.None => { return }
        }
    }
}
async fn main() {
    defer { discard write_text("main cleaned\n") }
    let pending = produce()
    sleep_ms(1).await
    let values = pending.await
    fail(values).await
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
        assert_eq!(lines.len(), 10, "{text}");
        assert!(
            lines[..8].iter().all(|line| *line == "child cleaned"),
            "{text}"
        );
        assert_eq!(&lines[8..], ["receiver cleaned", "main cleaned"]);
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("assertion failed"), "{diagnostic}");
        assert_eq!(diagnostic.matches("RuntimeFault:").count(), 1);
    }
}
