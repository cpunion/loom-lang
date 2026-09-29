use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn typed_inputs_prove_contracts_without_rechecking_construction() {
    let package = "compiler/examples/invariant_contracts";
    success(&common::loom(&["check", package]));
    success(&common::loom(&["test", package]));
    success(&common::loom(&["run", package]));
    let directory = tempfile::tempdir().unwrap();
    let artifact = common::executable(directory.path(), "invariant-contracts");
    let ir_path = directory.path().join("invariants.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package,
                "--output",
                artifact.to_str().unwrap(),
                "--emit-ir",
                ir_path.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(
            &Command::new(&artifact)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        if level == "0" {
            let ir = fs::read_to_string(&ir_path).unwrap();
            assert_eq!(
                ir.matches("icmp sgt i64").count(),
                2,
                "only the two dynamic construction predicates remain"
            );
            assert_eq!(ir.matches("icmp sge i64").count(), 1);
        }
    }
}
