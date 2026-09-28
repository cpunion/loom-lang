use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

mod common;
use common::success;

#[test]
fn source_standard_streams_preserve_binary_data_and_keep_text_strict() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("main.loom");
    fs::write(
        &source,
        r#"
import std.io.read_bytes
import std.io.read_text
import std.io.write_bytes
import std.io.write_text
import std.io.write_error
import std.io.ReadError
import std.bytes.length
import std.bytes.new
import std.process.arguments
import std.list.get
import std.result.Result

fn main() {
    if get(arguments(), 1) == "text" {
        match read_text() {
            Result.Ok(text) => {
                discard write_text(text)
            }
            Result.Err(error) => {
                assert error == ReadError.Utf8
                discard write_error("invalid UTF-8")
            }
        }
        return
    }
    let bytes = match read_bytes() {
        Result.Ok(value) => value
        Result.Err(_) => {
            assert false
            return
        }
    }
    assert match write_bytes(bytes) {
        Result.Ok(count) => count == length(bytes)
        Result.Err(_) => false
    }
    assert match write_error(bytes) {
        Result.Ok(count) => count == length(bytes)
        Result.Err(_) => false
    }
    // EOF is distinct from a closed descriptor. Empty writes remain valid.
    assert match read_bytes() {
        Result.Ok(value) => length(value) == 0
        Result.Err(_) => false
    }
    assert match write_bytes(new()) {
        Result.Ok(count) => count == 0
        Result.Err(_) => false
    }
    assert match write_error(new()) {
        Result.Ok(count) => count == 0
        Result.Err(_) => false
    }
}
"#,
    )
    .unwrap();
    let artifact = common::executable(directory.path(), "streams");
    let run = |mode, bytes: &[u8]| {
        let mut child = Command::new(&artifact)
            .arg(mode)
            .env("LOOM_GC_STRESS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(bytes).unwrap();
        child.wait_with_output().unwrap()
    };
    for level in ["0", "2"] {
        success(
            &common::command(&["build"])
                .arg(directory.path())
                .arg("--output")
                .arg(&artifact)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        for input in [vec![], (0..20_000).map(|index| index as u8).collect()] {
            let output = run("binary", &input);
            success(&output);
            assert_eq!(output.stdout, input);
            assert_eq!(output.stderr, input);
        }
        let text = "Hello, 雪!\n".as_bytes();
        let output = run("text", text);
        success(&output);
        assert_eq!(output.stdout, text);
        assert!(output.stderr.is_empty());
        let invalid = run("text", &[0, 255]);
        success(&invalid);
        assert!(invalid.stdout.is_empty());
        assert_eq!(invalid.stderr, b"invalid UTF-8");
    }
}
