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
                "task transfer failed to drain: {:?}",
                child.wait_with_output()
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn tasks_cross_generic_calls_and_completed_producers_keep_their_results() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.list.new
import std.list.push
import std.list.get
import std.list.length
import std.text.concat
import std.time.sleep_ms
import std.io.write_text

record Packet { text Text values List[Int] }

async fn number(value Int) Int { value }
fn forward[T](task Task[T]) Task[T] { task }
fn identity[T](value T) T { value }
fn factory(value Int) Task[Int] { number(value) }
async fn sum(first Task[Int], second Task[Int]) Int { first.await + second.await }
async fn tick() {}

async fn packet(trace List[Int]) Packet {
    push(trace, 2)
    let text = concat("managed ", "result")
    sleep_ms(1).await
    Packet { text = text values = [7, 11] }
}
async fn prepare(trace List[Int]) Task[Packet] {
    push(trace, 1)
    packet(trace)
}
async fn collect(outer Task[Task[Packet]]) Packet { outer.await.await }
async fn with_cleanup(trace List[Int]) Task[Int] {
    defer { push(trace, 3) }
    return number(19)
}

async fn main() {
    let trace = new[Int]()
    let outer = prepare(trace)
    tick().await
    // prepare has completed before collect adopts it, but its returned packet
    // still belongs to prepare until collect extracts the outer result.
    assert get(trace, 0) == 1
    let value = collect(identity(outer)).await
    discard concat("move ", "after extraction")
    assert value.text == "managed result"
    assert get(value.values, 0) + get(value.values, 1) == 18
    let first = factory(23)
    assert sum(forward(first), factory(29)).await == 52
    assert with_cleanup(trace).await.await == 19
    assert length(trace) == 3 && get(trace, 2) == 3
    discard write_text("transfers finished\n")
}

test async fn generic_transfer() {
    assert sum(identity(factory(3)), forward(factory(4))).await == 7
}
"#,
    )
    .unwrap();
    success(&common::loom(&["check", package.path().to_str().unwrap()]));
    let executable = common::executable(package.path(), "transfers");
    let ir = package.path().join("transfers.ll");
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
        let output = run(&executable);
        success(&output);
        assert_eq!(output.stdout, b"transfers finished\n");
        assert!(output.stderr.is_empty());
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(ir.contains("loom_rt_task_adopt") && ir.contains("loom_rt_task_return"));
        assert!(!ir.contains("llvm.coro") && !ir.contains("universal"));
    }
    success(&common::loom(&["test", package.path().to_str().unwrap()]));
}

#[test]
fn failure_cancels_transferred_completed_producers_and_waiting_descendants() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.time.sleep_ms
import std.io.write_text

async fn long_wait() Int {
    sleep_ms(600000).await
    discard write_text("cancelled descendant ran\n")
    1
}
async fn prepare() Task[Int] { long_wait() }
async fn fail(outer Task[Task[Int]]) {
    assert false
    discard outer.await.await
}
async fn main() {
    let outer = prepare()
    sleep_ms(1).await
    fail(outer).await
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "cancel-transfers");
    success(&common::loom(&[
        "build",
        package.path().to_str().unwrap(),
        "--output",
        executable.to_str().unwrap(),
    ]));
    let output = run(&executable);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("assertion failed"), "{diagnostic}");
    assert!(diagnostic.contains("task created here"), "{diagnostic}");
    assert_eq!(diagnostic.matches("RuntimeFault:").count(), 1);
}
