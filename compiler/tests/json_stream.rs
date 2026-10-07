use std::process::Command;

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
            success(
                &Command::new(&artifact)
                    .env("LOOM_GC_STRESS", "1")
                    .output()
                    .unwrap(),
            );
        }
    }
    let example = "compiler/examples/json_stream";
    for mode in ["check", "test", "run"] {
        success(&common::loom(&[mode, example]));
    }
    for level in ["0", "2"] {
        success(
            &common::command(&["build", example, "--output", artifact.to_str().unwrap()])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &Command::new(&artifact)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
}
