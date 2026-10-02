use std::process::Command;
mod common;
use common::success;

#[test]
fn source_iterators_keep_pull_order_sharing_and_fault_cleanup() {
    let package = "compiler/examples/iterators";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "iterators");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
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
