use std::fs;
mod common;
use common::{run_tasks, success};

const SOURCE: &str = r#"
import std.io.write_text
import std.time.sleep_ms
import std.list.length

async fn number(value Int) Int {
    sleep_ms(0).await
    value
}
async fn label() Text { "ready" }
async fn unit() { sleep_ms(0).await }
async fn nested() Task[Text] { label() }
async fn ready[T](value T) T { value }
fn blank[T]() List[T] { [] }
fn generic_tasks[T]() (Task[List[T]], Task[Int]) {
    (ready(blank[T]()), ready(2))
}

fn make() (Task[Int], Task[Text]) {
    discard write_text("make|")
    (number(3), label())
}
fn empty_tasks[Ts...](values Ts...) (Ts...) { values }
fn all(value Int) Int { value + 100 }
async fn typed() (Int, Text) { make().await }
async fn inferred_tuple() (List[Int], Int) { generic_tasks().await }
async fn empty_list[T]() List[T] { [] }
async fn typed_scalar() List[Int] { empty_list().await }

async fn exercise() {
    assert all(2) == 102
    let all = 99
    assert all == 99

    let tasks = make()
    let first, second = tasks.await
    assert first == 3 && second == "ready"

    let next, text = make().await
    assert next == 3 && text == "ready"
    let third, fourth = typed().await
    assert third == 3 && fourth == "ready"
    let blank_values, count = inferred_tuple().await
    assert length(blank_values) == 0 && count == 2
    discard empty_tasks().await

    let single = (number(7),).await
    assert single.0 == 7
    assert (number(8)).await == 8

    let pair = (unit(), nested()).await
    assert pair.1.await == "ready"
    assert length(typed_scalar().await) == 0
}

async fn main() {
    exercise().await
    discard write_text("done\n")
}

test async fn tuple_await_handles_all_arities_without_an_import() {
    exercise().await
}
"#;

#[test]
fn tuple_await_uses_source_all_without_import_or_name_capture() {
    let package = tempfile::tempdir().unwrap();
    fs::write(package.path().join("main.loom"), SOURCE).unwrap();
    success(&common::loom(&["check", package.path().to_str().unwrap()]));
    success(&common::loom(&["test", package.path().to_str().unwrap()]));
    let executable = common::executable(package.path(), "tuple-await");
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
        let output = run_tasks(&executable);
        success(&output);
        assert_eq!(output.stdout, b"make|make|make|done\n", "O{level}");
        assert!(output.stderr.is_empty(), "O{level}");
    }
}

#[test]
fn tuple_await_rejects_non_task_elements() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        "import std.time.sleep_ms\nasync fn main() { discard (sleep_ms(0), 1).await }\n",
    )
    .unwrap();
    let output = common::loom(&["check", package.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("tuple await requires Task values"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn tuple_await_consumes_each_handle_once() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        "async fn item() Int { 1 }\nasync fn main() { let tasks = (item(), item())\ndiscard tasks.await\ndiscard tasks.await }\n",
    )
    .unwrap();
    let output = common::loom(&["check", package.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("this task was already awaited or transferred"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn tuple_await_fault_drains_siblings() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.io.write_text
import std.time.sleep_ms
async fn stalled() Int {
    defer { discard write_text("drained|") }
    sleep_ms(600000).await
    1
}
async fn failed() Int {
    assert false
    0
}
async fn main() {
    let tasks = (stalled(), failed())
    discard tasks.await
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "tuple-await-fault");
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
        let output = run_tasks(&executable);
        assert_eq!(output.status.code(), Some(1), "O{level}");
        assert_eq!(output.stdout, b"drained|", "O{level}");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostic.contains("assertion failed"),
            "O{level}: {diagnostic}"
        );
        assert_eq!(diagnostic.matches("RuntimeFault:").count(), 1);
    }
}
