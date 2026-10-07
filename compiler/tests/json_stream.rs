mod common;
use common::success;

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
        success(&common::run_task_command(&mut common::command(&[
            mode, example,
        ])));
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
