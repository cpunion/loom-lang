use std::{
    fs,
    io::{self, Read, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

mod common;
use common::{loom, success};

const PAYLOAD_BYTES: usize = 256 * 1024;
const INPUT_MODE: &str = "literal input 雪 *? '$;";
const RUN_INPUT_TEXT: &str = "text input\0雪🙂 *? '$;\n";
const RUN_INPUT_REPEATS: usize = 16384;

#[test]
#[ignore = "subprocess fixture for native process input"]
fn binary_child_fixture() {
    let args = std::env::args().collect::<Vec<_>>();
    let mode = args.last().unwrap();
    if mode == "configured" {
        assert_eq!(
            std::env::var("LOOM_CAPTURE_INPUT_VALUE").unwrap(),
            "child 雪"
        );
        assert_eq!(fs::read_to_string("marker").unwrap(), "child directory");
    }
    let payload = (0..PAYLOAD_BYTES)
        .map(|index| index as u8)
        .collect::<Vec<_>>();
    // Exceed pipe capacity on both outputs before consuming any input. A parent
    // that writes stdin or drains outputs sequentially deadlocks here.
    io::stdout().write_all(&payload).unwrap();
    io::stdout().flush().unwrap();
    io::stderr()
        .write_all(&payload.iter().map(|byte| 255 - byte).collect::<Vec<_>>())
        .unwrap();
    io::stderr().flush().unwrap();
    if mode == INPUT_MODE || mode == "configured" {
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input).unwrap();
        assert_eq!(input, payload);
        io::stdout().write_all(&input).unwrap();
        io::stdout().flush().unwrap();
    } else if mode == "empty" {
        assert_eq!(io::stdin().read(&mut [0]).unwrap(), 0);
    } else if mode == "run-input" {
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input).unwrap();
        assert_eq!(input, RUN_INPUT_TEXT.repeat(RUN_INPUT_REPEATS).as_bytes());
    } else {
        assert_eq!(mode, "early");
    }
    std::process::exit(7);
}

#[test]
fn native_run_input_reports_early_stdin_close_without_terminating_parent() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    let input_path = directory.path().join("input.txt");
    fs::write(&input_path, RUN_INPUT_TEXT.repeat(RUN_INPUT_REPEATS)).unwrap();
    fs::write(
        source.join("main.loom"),
        format!(
            r#"
import std.process.run_input
import std.process.arguments
import std.process.SpawnError
import std.file.read_text
import std.list.new
import std.list.push
import std.list.get
import std.result.Result
fn main() {{
    let input = match read_text({input_path:?}) {{
        Result.Ok(value) => value,
        Result.Err(_) => {{
            assert false
            ""
        }}
    }}
    let mode = get(arguments(), 1)
    let args = new[Text]()
    push(args, {executable:?})
    push(args, "--ignored")
    push(args, "--exact")
    push(args, "binary_child_fixture")
    push(args, "--nocapture")
    push(args, "--skip")
    push(args, mode)
    match run_input(args, input) {{
        Result.Ok(code) => {{ assert mode == "run-input" && code == 7 }}
        Result.Err(error) => {{ assert mode == "early" && error == SpawnError.Failed }}
    }}
}}
"#,
            input_path = input_path.to_str().unwrap(),
            executable = std::env::current_exe().unwrap().to_str().unwrap(),
        ),
    )
    .unwrap();
    let artifact = common::executable(directory.path(), "run input parent");
    success(&loom(&[
        "build",
        source.to_str().unwrap(),
        "--output",
        artifact.to_str().unwrap(),
    ]));
    let payload = (0..PAYLOAD_BYTES)
        .map(|index| index as u8)
        .collect::<Vec<_>>();
    let stderr = payload.iter().map(|byte| 255 - byte).collect::<Vec<_>>();
    for mode in ["run-input", "early"] {
        // Drain the inherited streams while the Loom parent writes stdin. The
        // early-close case must return Failed instead of a native SIGPIPE exit.
        let output = Command::new(&artifact)
            .arg(mode)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "run_input mode {mode}: native parent status {} (stdout {} bytes, stderr {} bytes)",
            output.status,
            output.stdout.len(),
            output.stderr.len(),
        );
        assert!(output.stdout.ends_with(&payload));
        assert_eq!(output.stderr, stderr);
    }
}

#[test]
fn native_capture_input_preserves_binary_streams_options_and_early_exit() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let working = directory.path().join("working 雪");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&working).unwrap();
    fs::write(working.join("marker"), "child directory").unwrap();
    let payload = directory.path().join("payload.bin");
    fs::write(
        &payload,
        (0..PAYLOAD_BYTES)
            .map(|index| index as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    fs::write(
        source.join("main.loom"),
        format!(
            r#"
import std.process.capture
import std.process.capture_input
import std.process.Options
import std.process.EnvChange
import std.process.Output
import std.process.ExitStatus
import std.process.SpawnError
import std.file.read_bytes
import std.bytes.new
import std.bytes.get
import std.bytes.length
import std.list.new
import std.list.push
import std.list.set
import std.option.Option
import std.result.Result
fn check(captured Result[Output, SpawnError], copies Int) {{
    match captured {{
        Result.Err(_) => {{ assert false }}
        Result.Ok(output) => {{
            assert match output.status {{ ExitStatus.Exited(code) => code == 7, _ => false }}
            let count = {payload_bytes}
            let start = length(output.stdout) - count * copies
            assert start >= 0
            assert length(output.stderr) == count
            var index = 0
            while index < count * copies {{
                assert get(output.stdout, start + index) == index % 256
                index = index + 1
            }}
            index = 0
            while index < count {{
                assert get(output.stderr, index) == 255 - index % 256
                index = index + 1
            }}
        }}
    }}
}}
fn main() {{
    let input = match read_bytes({payload:?}) {{
        Result.Ok(value) => value,
        Result.Err(_) => {{
            assert false
            std.bytes.new()
        }}
    }}
    let args = new[Text]()
    push(args, {executable:?})
    push(args, "--ignored")
    push(args, "--exact")
    push(args, "binary_child_fixture")
    push(args, "--nocapture")
    push(args, "--skip")
    push(args, {input_mode:?})
    check(capture_input(args, input), 2)
    assert length(input) == {payload_bytes}
    assert get(input, 0) == 0 && get(input, 255) == 255
    set(args, 6, "configured")
    let changes = new[EnvChange]()
    push(changes, EnvChange.Set("LOOM_CAPTURE_INPUT_VALUE", "child 雪"))
    check(capture_input(args, input, Options {{ directory = Option.Some({working:?}) clear_environment = false environment = changes }}), 2)
    set(args, 6, "empty")
    check(capture(args), 1)
    check(capture_input(args, std.bytes.new()), 1)
    set(args, 6, "early")
    check(capture_input(args, input), 1)
}}
"#,
            payload_bytes = PAYLOAD_BYTES,
            payload = payload.to_str().unwrap(),
            executable = std::env::current_exe().unwrap().to_str().unwrap(),
            input_mode = INPUT_MODE,
            working = working.to_str().unwrap(),
        ),
    )
    .unwrap();
    let artifact = common::executable(directory.path(), "capture input parent");
    success(&loom(&[
        "build",
        source.to_str().unwrap(),
        "--output",
        artifact.to_str().unwrap(),
    ]));
    let mut child = Command::new(artifact)
        .env("LOOM_GC_STRESS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("capture_input did not finish: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    success(&output);
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}
