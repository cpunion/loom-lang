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
