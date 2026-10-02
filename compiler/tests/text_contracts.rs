use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn text_contracts_prove_without_runtime_postcondition_helpers() {
    let package = "compiler/examples/text_contracts";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "text-contracts");
    let ir = temporary.path().join("text-contracts.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .arg("--emit-ir")
                .arg(&ir)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        let emitted = fs::read_to_string(&ir).unwrap();
        for marker in ["91827365", "81726354", "71928364", "61728394"] {
            assert!(
                !emitted.contains(marker),
                "proof-only Text helper became a runtime root"
            );
        }
        success(
            &Command::new(&executable)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
}
