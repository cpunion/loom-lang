use std::fs;

mod common;
use common::{loom, success};

#[test]
fn nonempty_transformations_compose_in_real_programs_and_workers() {
    let package = "compiler/examples/nonempty_pipeline";
    success(&loom(&["check", package]));
    success(&loom(&["run", package]));
    let temporary = tempfile::tempdir().unwrap();
    let artifact = common::executable(temporary.path(), "nonempty");
    for level in ["0", "2"] {
        success(
            &common::command(&["test", package])
                .args(["--no-run", "--output"])
                .arg(&artifact)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&artifact));
    }
}

#[test]
fn nonempty_hides_its_shared_list_from_other_packages() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    fs::write(
        &source,
        "import std.list.nonempty.new\nimport std.list.nonempty.first\nfn main() { let values = new(1)\nassert first(values) == 1 }",
    )
    .unwrap();
    let checked = loom(&["check", temporary.path().to_str().unwrap()]);
    assert_eq!(checked.status.code(), Some(0), "{checked:?}");

    fs::write(
        &source,
        "import std.list.nonempty.new\nfn main() { let values = new(1)\ndiscard values.state.values }",
    )
    .unwrap();
    let rejected = loom(&["check", temporary.path().to_str().unwrap()]);
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("cannot inspect a private type"),
        "{rejected:?}"
    );
}
