mod common;
use common::{run_tasks, success};

#[test]
fn notifications_resume_typed_loom_frames_without_container_polling() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "observations");
    success(&common::loom(&["check", "compiler/std/task"]));
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "test",
                "compiler/std/task",
                "--no-run",
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = run_tasks(&executable);
        success(&output);
        // Standalone test executables are silent on success; the CLI prints
        // the test count after it runs them.
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn public_outcomes_and_cancellation_run_as_an_ordinary_native_program() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "outcomes");
    success(&common::loom(&["check", "compiler/examples/task_outcomes"]));
    success(&common::loom(&[
        "build",
        "compiler/examples/task_outcomes",
        "--output",
        executable.to_str().unwrap(),
    ]));
    let output = run_tasks(&executable);
    success(&output);
    assert_eq!(output.stdout, b"outcomes finished\n");
    assert!(output.stderr.is_empty());
}
