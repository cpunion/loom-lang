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

#[test]
fn float_loop_induction_rechecks_edits_and_rejects_ieee_counterexamples() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    let program = format!(
        "{}\nfn main() {{\n    float_loop_exercise()\n}}\n",
        include_str!("../examples/loop_contracts/floats.loom")
    );
    let cache = package.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    fs::write(&source, &program).unwrap();
    success(&check());
    success(&check());
    for changed in [
        program.replace("current = current / 2.0", "current = -current"),
        program.replace("current = current / 2.0", "current = 0.0 / 0.0"),
        r#"
fn invalid(value Float) Float
requires value > 0.0
ensures result > 0.0 {
    var current = value
    var index = 0
    while index < 1 {
        current = current / 2.0
        index = index + 1
    }
    current
}
"#
        .to_owned(),
        r#"
fn invalid(value Float) Float
requires value == 0.0
ensures 1.0 / result == 1.0 / value {
    var current = value
    var index = 0
    while index < 1 {
        current = -current
        index = index + 1
    }
    current
}
"#
        .to_owned(),
        r#"
fn invalid(value Float) Float
requires value >= 0.0 && value <= 1.0
ensures result >= 0.0 {
    var current = value
    var index = 0
    while index < 2 {
        current = if index == 1 { -1.0 } else { 0.0 }
        index = index + 1
    }
    current
}
"#
        .to_owned(),
    ] {
        assert_ne!(changed, program, "test edit did not apply");
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(!output.status.success(), "unsound Float loop: {changed}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("proved") || error.contains("SMT"), "{error}");
    }
    fs::write(&source, &program).unwrap();
    success(&check());
}
