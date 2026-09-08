use std::{fs, process::Command};
mod common;
use common::success;

const SOURCE: &str = r#"
import std.io.write_text
import std.text.concat
import std.list.new
import std.list.push
import std.list.get
import std.result.Result

record Packet {
    text Text
    values List[Int]
}

async fn packet(label Text, number Int) Packet {
    discard write_text(label)
    let values = new[Int]()
    discard push(values, number)
    Packet { text = concat(label, "retained") values = values }
}

async fn number(value Int) Int { value + 1 }

async fn positive(value Int) Result[Int, Text] {
    if value > 0 { Result.Ok(value) } else { Result.Err("negative") }
}

async fn propagated() Result[Int, Text] {
    let value = positive(7).await?
    Result.Ok(value + 1)
}

async fn calculate() Int {
    let first = packet("first|", 17)
    let second = packet("second|", 23)
    discard write_text("created|")
    let left = first.await
    let right = second.await
    discard concat("move", " completed result roots")
    assert left.text == "first|retained"
    assert right.text == "second|retained"
    var total = get(left.values, 0) + get(right.values, 0)
    var index = 0
    while index < 3 {
        let next = number(index)
        total = total + next.await
        index = index + 1
    }
    let extra = if total == 46 { number(3).await } else { number(99).await }
    let outcome = propagated().await
    match outcome {
        Result.Ok(value) => total + extra + value
        Result.Err(_) => 0
    }
}

async fn main() {
    assert calculate().await == 58
    discard write_text("done|")
}

test async fn source_async_test() {
    assert number(40).await == 41
    match propagated().await {
        Result.Ok(value) => { assert value == 8 }
        Result.Err(_) => { assert false }
    }
}
"#;

#[test]
fn source_tasks_lower_to_native_functions_and_preserve_live_values() {
    let package = tempfile::tempdir().unwrap();
    fs::write(package.path().join("main.loom"), SOURCE).unwrap();
    success(&common::loom(&["check", package.path().to_str().unwrap()]));
    let executable = common::executable(package.path(), "tasks");
    let ir = package.path().join("tasks.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package.path().to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        for stress in ["0", "1"] {
            let output = Command::new(&executable)
                .env("LOOM_GC_STRESS", stress)
                .output()
                .unwrap();
            success(&output);
            assert_eq!(
                output.stdout, b"created|first|second|done|",
                "O{level}/GC{stress}"
            );
            assert!(output.stderr.is_empty());
        }
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(ir.contains("loom_rt_task_create") && ir.contains("loom_rt_task_run"));
        assert!(ir.contains("uwtable(sync)"));
        assert!(!ir.contains("llvm.coro") && !ir.contains("universal"));
    }
    let output = common::loom(&["test", package.path().to_str().unwrap()]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 tests passed"));
}

#[test]
fn source_task_preconditions_fault_in_child_and_drain_before_reporting() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.io.write_text

async fn required(value Int) Int requires value > 0 {
    discard write_text("invalid body ran|")
    value
}
async fn sibling() { discard write_text("cancelled sibling ran|") }
async fn main() {
    let failed = required(0)
    let unused = sibling()
    discard write_text("created|")
    discard failed.await
    unused.await
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "task-failure");
    success(&common::loom(&[
        "build",
        package.path().to_str().unwrap(),
        "--output",
        executable.to_str().unwrap(),
    ]));
    let output = Command::new(&executable)
        .env("LOOM_GC_STRESS", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"created|");
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("precondition failed"), "{diagnostic}");
    assert!(
        diagnostic.contains("main.loom:10:18: task created here"),
        "{diagnostic}"
    );
    assert_eq!(
        diagnostic.matches("RuntimeFault:").count(),
        1,
        "{diagnostic}"
    );
}
