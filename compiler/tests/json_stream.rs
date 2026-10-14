mod common;
use common::success;

#[test]
fn json_line_adapter_cannot_retain_an_existing_scoped_borrow() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::write(
        temporary.path().join("main.loom"),
        r#"
import std.stream.Stream
import std.resource.MustScope
import std.resource.Dispose
import std.json.stream.lines
import std.option.Option
import std.result.Result

record OwnedChunks {
}

impl MustScope for OwnedChunks {
}

impl Dispose for OwnedChunks {
    fn dispose(self OwnedChunks) {
    }
}

impl Stream for OwnedChunks {
    type Item = Bytes

    async fn next(self OwnedChunks) Option[Bytes] {
        Option.None
    }
}

fn packet(value Bytes) Result[Bytes, Text] {
    Result.Ok(value)
}

async fn main() {
    scoped input = OwnedChunks {
    }
    scoped retained = lines(input, packet)
    discard retained.next().await
}
"#,
    )
    .unwrap();
    let output = common::loom(&["check", temporary.path().to_str().unwrap()]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("borrow"), "{error}");
}

#[test]
fn json_range_feeding_faults_before_reading_invalid_ranges() {
    let temporary = tempfile::tempdir().unwrap();
    let artifact = common::executable(temporary.path(), "json-input-range");
    for (start, end) in [(-1, 4), (2, 1), (0, 5)] {
        std::fs::write(
            temporary.path().join("main.loom"),
            format!(
                r#"
import std.json.decoder
import std.json.feed
import std.text.to_bytes

fn main() {{
    discard feed(decoder(), to_bytes("true"), {start}, {end})
}}
"#
            ),
        )
        .unwrap();
        for level in ["0", "2"] {
            success(
                &common::command(&["build", temporary.path().to_str().unwrap()])
                    .arg("--output")
                    .arg(&artifact)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            let output = common::run_tasks(&artifact);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("assertion failed"));
        }
    }
}

#[test]
fn incremental_json_parses_fragmented_input_with_real_io_and_moving_gc() {
    let temporary = tempfile::tempdir().unwrap();
    let artifact = common::executable(temporary.path(), "json-stream");
    for package in ["compiler/std/json", "compiler/std/json/stream"] {
        success(&common::loom(&["check", package]));
        for level in ["0", "2"] {
            success(
                &common::command(&["test", package, "--no-run"])
                    .arg("--output")
                    .arg(&artifact)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            success(&common::run_tasks(&artifact));
        }
    }
    let example = "compiler/examples/json_stream";
    for mode in ["check", "test", "run"] {
        // Compiler/linker time is not Task drain time. Emitted O0/O2 programs
        // below retain the bounded run_tasks check.
        success(&common::loom(&[mode, example]));
    }
    for level in ["0", "2"] {
        success(
            &common::command(&["build", example, "--output", artifact.to_str().unwrap()])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&artifact));
    }
}

#[test]
fn json_output_checks_positive_chunk_sizes_even_when_discarded() {
    let temporary = tempfile::tempdir().unwrap();
    let artifact = common::executable(temporary.path(), "json-output-contract");
    for source in [
        r#"
import std.json.encoder
import std.json.Value

fn main() {
    discard encoder(Value.Null, 0)
}
"#,
        r#"
import std.json.stream.write
import std.json.Value
import std.result.Result
import std.bytes.length

async fn main() {
    let sink = async fn(chunk Bytes) Result[Int, Text] {
        assert false
        Result.Ok(length(chunk))
    }
    discard write(Value.Null, 0, sink).await
}
"#,
    ] {
        std::fs::write(temporary.path().join("main.loom"), source).unwrap();
        for level in ["0", "2"] {
            success(
                &common::command(&["build", temporary.path().to_str().unwrap()])
                    .arg("--output")
                    .arg(&artifact)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            let output = common::run_tasks(&artifact);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("precondition failed"));
        }
    }
}
