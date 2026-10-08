use std::{fs, process::Command};

mod common;
use common::success;

#[test]
fn inferred_loop_contracts_compile_without_runtime_proof_helpers() {
    let package = "compiler/examples/loop_contracts";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let directory = tempfile::tempdir().unwrap();
    let artifact = common::executable(directory.path(), "loop-contracts");
    let ir = directory.path().join("loops.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&artifact)
                .arg("--emit-ir")
                .arg(&ir)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        assert!(
            !fs::read_to_string(&ir).unwrap().contains("91827365"),
            "a proof-only helper became native code"
        );
        success(
            &Command::new(&artifact)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
}
