use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn sequence_lengths_track_shared_state_without_runtime_postconditions() {
    list_contract_package("compiler/examples/list_contracts");
    list_contract_package("compiler/examples/list_elements");
    list_contract_package("compiler/examples/byte_contracts");
}

#[test]
fn changed_byte_proofs_do_not_reuse_cached_postconditions() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    let cache = temporary.path().join("cache");
    let program = include_str!("../examples/byte_contracts/main.loom");
    let check = || {
        common::loom(&[
            "check",
            temporary.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    fs::write(&source, program).unwrap();
    success(&check());
    success(&check());
    for changed in [
        program.replace("result <= 255", "result <= 254"),
        program.replace("old(length(value)) + 1", "old(length(value)) + 2"),
        program.replace("set(value, index, byte)", "set(value, index, 0)"),
    ] {
        assert_ne!(changed, program);
        fs::write(&source, changed).unwrap();
        let output = check();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved"),
            "{error}"
        );
    }
}

fn list_contract_package(package: &str) {
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
        for proof_only in [
            "62917384",
            "81364279",
            "contract_old",
            "entry_list_len",
            "entry_list_get",
        ] {
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
