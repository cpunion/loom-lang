use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn list_lengths_track_shared_state_without_runtime_postconditions() {
    let package = "compiler/examples/list_contracts";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "list-contracts");
    let ir = temporary.path().join("list-contracts.ll");
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
        let ir = fs::read_to_string(&ir).unwrap();
        for proof_only in ["62917384", "contract_old", "entry_list_len"] {
            assert!(!ir.contains(proof_only));
        }
        success(
            &Command::new(&executable)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
}
