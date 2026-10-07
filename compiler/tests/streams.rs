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
            success(
                &Command::new(&executable)
                    .env("LOOM_GC_STRESS", "1")
                    .output()
                    .unwrap(),
            );
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
