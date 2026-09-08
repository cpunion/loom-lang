use std::fs;
mod common;
use common::{run_tasks, success};

#[test]
fn task_aggregate_fields_and_multiple_returned_subtrees_execute_natively() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "aggregates");
    success(&common::loom(&[
        "check",
        "compiler/examples/task_aggregates",
    ]));
    success(&common::loom(&[
        "test",
        "compiler/examples/task_aggregates",
    ]));
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                "compiler/examples/task_aggregates",
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = run_tasks(&executable);
        success(&output);
        assert_eq!(output.stdout, b"aggregates finished\n");
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn aggregate_adoption_drains_all_returned_children_before_parent_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.loom"),
        r#"
import std.time.sleep_ms
import std.io.write_text
async fn child(name Text) Int {
    defer { discard write_text(name) }
    sleep_ms(600000).await
    1
}
async fn produce() (Task[Int], Task[Int]) {
    (child("left cleaned\n"), child("right cleaned\n"))
}
async fn fail(pair (Task[Int], Task[Int])) Int {
    defer { discard write_text("receiver cleaned\n") }
    sleep_ms(1).await
    assert false
    pair.0.await + pair.1.await
}
async fn main() {
    defer { discard write_text("main cleaned\n") }
    let pending = produce()
    sleep_ms(1).await
    let pair = pending.await
    discard fail(pair).await
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
            diagnostic.contains("main.loom:23:13: task created here"),
            "{diagnostic}"
        );
        assert_eq!(diagnostic.matches("RuntimeFault:").count(), 1);
    }
}
