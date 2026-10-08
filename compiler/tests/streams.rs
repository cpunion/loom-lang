use std::process::Command;

mod common;
use common::success;

#[test]
fn source_streams_keep_typed_pulls_real_io_and_task_cleanup() {
    let temporary = tempfile::tempdir().unwrap();
    for package in ["compiler/std/stream", "compiler/std/net/tcp/chunks"] {
        success(&common::loom(&["check", package]));
        let executable = common::executable(temporary.path(), "stream-tests");
        for level in ["0", "2"] {
            success(
                &common::command(&["test", package, "--no-run"])
                    .arg("--output")
                    .arg(&executable)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            success(&common::run_tasks(&executable));
        }
    }
    let example = "compiler/examples/streams";
    for mode in ["check", "test", "run"] {
        success(&common::loom(&[mode, example]));
    }
    let executable = common::executable(temporary.path(), "stream-example");
    success(
        &common::command(&["build", example])
            .arg("--output")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = Command::new(executable)
        .env("LOOM_GC_STRESS", "1")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"item 0 ready\nitem 1 ready\nitem 2 ready\n");
}

#[test]
fn scoped_async_file_pipelines_keep_errors_and_stop_before_the_suffix() {
    let package = "compiler/examples/stream_lines";
    for mode in ["check", "test"] {
        success(&common::loom(&[mode, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "stream-lines");
    let input = temporary.path().join("input.txt");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        for (contents, expected) in [
            (
                &b"\nfirst\r\n\nsecond\n\xff\n"[..],
                Some(&b"first\nsecond\n"[..]),
            ),
            (&b"\nfirst\n\xff\n"[..], None),
        ] {
            std::fs::write(&input, contents).unwrap();
            let output = common::run_task_command(
                Command::new(&executable)
                    .arg(&input)
                    .env("LOOM_GC_STRESS", "1"),
            );
            match expected {
                Some(text) => {
                    success(&output);
                    assert_eq!(output.stdout, text);
                }
                None => {
                    assert_eq!(output.status.code(), Some(1));
                    assert_eq!(output.stderr, b"cannot read UTF-8 lines\n");
                }
            }
        }
    }
    std::fs::write(&input, "first\nsecond\n").unwrap();
    let output = common::command(&["run", package])
        .arg("--")
        .arg(&input)
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"first\nsecond\n");
}
