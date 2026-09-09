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
fn public_outcomes_and_joins_run_as_ordinary_native_programs() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "outcomes");
    for (package, expected) in [
        ("compiler/examples/task_outcomes", "outcomes finished\n"),
        ("compiler/examples/task_joins", "joins finished\n"),
    ] {
        success(&common::loom(&["check", package]));
        success(&common::loom(&[
            "build",
            package,
            "--output",
            executable.to_str().unwrap(),
        ]));
        let output = run_tasks(&executable);
        success(&output);
        assert_eq!(output.stdout, expected.as_bytes());
        assert!(output.stderr.is_empty());
    }
}
