use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};
mod common;
use common::success;

const PAYLOAD: usize = 128 * 1024;

#[test]
#[ignore = "subprocess fixture"]
fn async_capture_child() {
    let directory = PathBuf::from(std::env::var_os("LOOM_ASYNC_CAPTURE_DIR").unwrap());
    assert_eq!(fs::read_to_string("marker").unwrap(), "child directory");
    assert_eq!(
        std::env::var("LOOM_ASYNC_CAPTURE_VALUE").unwrap(),
        "last=雪"
    );
    assert!(std::env::var_os("LOOM_ASYNC_CAPTURE_REMOVE").is_none());
    let cancel = std::env::var("LOOM_ASYNC_CAPTURE_MODE").unwrap() == "cancel";
    fs::write(
        directory.join(if cancel { "cancel-ready" } else { "ready" }),
        "ready",
    )
    .unwrap();
    if cancel {
        thread::sleep(Duration::from_millis(100));
        fs::write(directory.join("finished"), "done").unwrap();
    } else {
        // The owner must be able to acknowledge while capture is pending. A
        // bounded wait turns accidental synchronous capture into a test failure.
        let limit = Instant::now() + Duration::from_secs(10);
        while !directory.join("ack").exists() {
            assert!(
                Instant::now() < limit,
                "owner did not resume during capture"
            );
            thread::sleep(Duration::from_millis(2));
        }
        let bytes: Vec<u8> = (0..PAYLOAD).map(|index| index as u8).collect();
        std::io::stdout().write_all(&bytes).unwrap();
        std::io::stderr().write_all(&bytes).unwrap();
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input).unwrap();
        assert_eq!(input, bytes);
    }
    std::process::exit(7);
}

#[test]
fn process_capture_parks_snapshots_inputs_and_drains_native_work() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("package");
    let working = directory.path().join("child 雪🙂");
    fs::create_dir(&package).unwrap();
    fs::create_dir(&working).unwrap();
    fs::write(working.join("marker"), "child directory").unwrap();
    let payload = directory.path().join("payload.bin");
    fs::write(
        &payload,
        (0..PAYLOAD).map(|index| index as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    fs::write(
        package.join("main.loom"),
        r#"
import std.process.arguments
import std.process.tasks.capture
import std.process.tasks.capture_input
import std.process.Options
import std.process.EnvChange
import std.process.ExitStatus
import std.file.read_text
import std.file.read_bytes
import std.file.write_text
import std.bytes.get
import std.bytes.new
import std.bytes.set
import std.bytes.length
import std.list.set
import std.text.concat
import std.option.Option
import std.result.Result
import std.time.sleep_ms
import std.task.cancel
import std.task.Outcome

async fn ready(path Text) {
    var attempts = 0
    while attempts < 5000 {
        if match read_text(path) {
            Result.Ok(value) => value == "ready"
            Result.Err(_) => false
        } {
            return
        }
        sleep_ms(2).await
        attempts = attempts + 1
    }
    assert false
}

async fn main() {
    let arguments = arguments()
    let directory = arguments[2]
    let args = [arguments[1], "--ignored", "--exact", "async_capture_child", "--nocapture"]
    let changes = [
        EnvChange.Set("LOOM_ASYNC_CAPTURE_DIR", directory),
        EnvChange.Set("LOOM_ASYNC_CAPTURE_VALUE", "first"),
        EnvChange.Remove("LOOM_ASYNC_CAPTURE_VALUE"),
        EnvChange.Set("LOOM_ASYNC_CAPTURE_VALUE", "last=雪"),
        EnvChange.Remove("LOOM_ASYNC_CAPTURE_REMOVE"),
        EnvChange.Set("LOOM_ASYNC_CAPTURE_MODE", "capture")
    ]
    let options = Options {
        directory = Option.Some(arguments[3])
        clear_environment = false
        environment = changes
    }
    let input = match read_bytes(arguments[4]) {
        Result.Ok(bytes) => bytes
        Result.Err(_) => {
            assert false
            std.bytes.new()
        }
    }
    let pending = capture_input(args, input, options)
    ready(concat(directory, "/ready")).await
    // All native inputs were copied before the child became observable.
    std.bytes.set(input, 0, 99)
    std.list.set(args, 0, "invalid\0changed executable")
    std.list.set(changes, 3, EnvChange.Set("LOOM_ASYNC_CAPTURE_VALUE", "changed"))
    assert match write_text(concat(directory, "/ack"), "go") {
        Result.Ok(_) => true
        Result.Err(_) => false
    }
    let output = pending.await
    match output {
        Result.Ok(output) => {
            assert match output.status {
                ExitStatus.Exited(code) => code == 7
                _ => false
            }
            let start = length(output.stdout) - 131072
            assert start >= 0 && length(output.stderr) == 131072
            var index = 0
            while index < 131072 {
                assert get(output.stdout, start + index) == index % 256
                assert get(output.stderr, index) == index % 256
                index = index + 1
            }
        }
        Result.Err(_) => {
            assert false
        }
    }
    std.list.set(args, 0, arguments[1])
    std.list.set(changes, 3, EnvChange.Set("LOOM_ASYNC_CAPTURE_VALUE", "last=雪"))
    std.list.set(changes, 5, EnvChange.Set("LOOM_ASYNC_CAPTURE_MODE", "cancel"))
    let retiring = capture(args, options)
    ready(concat(directory, "/cancel-ready")).await
    match cancel(retiring) {
        Outcome.Cancelled => {}
        Outcome.Completed(_) => {}
        Outcome.Faulted(_) => {
            assert false
        }
    }
    assert match read_text(concat(directory, "/finished")) {
        Result.Ok(value) => value == "done"
        Result.Err(_) => false
    }
}
"#,
    )
    .unwrap();
    let executable = common::executable(directory.path(), "async-process");
    let ir = directory.path().join("async-process.ll");
    success(&common::loom(&["check", package.to_str().unwrap()]));
    success(&common::loom(&["test", "compiler/std/process/tasks"]));
    for level in ["0", "2"] {
        for marker in ["ready", "ack", "cancel-ready", "finished"] {
            let path = directory.path().join(marker);
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }
        success(
            &common::command(&[
                "build",
                package.to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = common::run_task_command(
            Command::new(&executable)
                .arg(std::env::current_exe().unwrap())
                .arg(directory.path())
                .arg(&working)
                .arg(&payload)
                .env("LOOM_GC_STRESS", "1")
                .env("LOOM_ASYNC_CAPTURE_REMOVE", "inherited"),
        );
        success(&output);
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(ir.contains("loom_rt_task_wait_process_capture"));
        assert!(ir.contains("loom_rt_task_process_capture_result"));
        assert!(!ir.contains("@loom_rt_process_capture"));
    }
}
