mod common;
use common::success;

#[test]
fn source_record_codecs_use_native_fields_and_checked_constructors() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "storage");
    let example = "compiler/examples/storage";
    success(&common::loom(&["check", example]));
    for level in ["0", "2"] {
        for (mode, package) in [
            ("test", "compiler/std/storage"),
            ("test", example),
            ("build", example),
        ] {
            let mut command = common::command(&[mode, package]);
            if mode == "test" {
                command.arg("--no-run");
            }
            success(
                &command
                    .arg("--output")
                    .arg(&executable)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            success(&common::run_tasks(&executable));
        }
    }
    success(&common::loom(&["run", example]));
}
