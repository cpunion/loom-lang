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
            let output = child.wait_with_output().unwrap();
            panic!("timer task did not finish/drain: {output:?}");
        }
        thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn source_timers_resume_typed_frames_and_evaluate_deadlines_once() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.time.monotonic_ns
import std.time.sleep_until_ns
import std.time.sleep_ns
import std.time.sleep_ms
import std.text.concat
import std.list.new
import std.list.push
import std.list.get
import std.list.length
import std.io.write_text

fn deadline(calls List[Int]) Int {
    push(calls, 1)
    monotonic_ns() + 20000000
}

async fn delayed(calls List[Int]) Text {
    let text = concat("rooted ", "across wait")
    let until = deadline(calls)
    sleep_until_ns(until).await
    assert monotonic_ns() >= until
    discard concat("move", " after wake")
    text
}

async fn main() {
    let calls = new[Int]()
    let first = delayed(calls)
    let second = sleep_ms(1)
    assert length(calls) == 0
    let text = first.await
    second.await
    assert text == "rooted across wait"
    assert length(calls) == 1 && get(calls, 0) == 1
    var index = 0
    while index < 3 {
        sleep_ns(1000000).await
        index = index + 1
    }
    sleep_until_ns(0).await
    discard write_text("timers finished\n")
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "timers");
    let ir = package.path().join("timers.ll");
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
        assert_eq!(output.stdout, b"timers finished\n");
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(ir.contains("loom_rt_task_wait_timer") && ir.contains("loom_rt_monotonic_ns"));
        assert!(!ir.contains("llvm.coro"));
    }
}

#[test]
fn parent_failure_drains_registered_timers_before_reporting() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.time.sleep_ms
import std.io.write_text

async fn long_wait() {
    sleep_ms(600000).await
    discard write_text("cancelled timer ran\n")
}
async fn failed() {
    sleep_ms(1).await
    assert false
}
async fn main() {
    let pending = long_wait()
    let error = failed()
    error.await
    pending.await
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "cancel-timer");
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
    assert!(
        diagnostic.contains("assertion failed") && diagnostic.contains("task created here"),
        "{diagnostic}"
    );
    assert_eq!(diagnostic.matches("RuntimeFault:").count(), 1);
}
