use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn solver_contracts_check_and_compile_without_a_runtime_solver() {
    let package = "compiler/examples/smt_contracts";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "smt-contracts");
    let ir = temporary.path().join("app.ll");
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
        assert!(!emitted.contains("process_capture"));
        success(
            &Command::new(&executable)
                .env("PATH", "")
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn solver_refutes_counterexamples_and_does_not_rescue_undefined_contracts() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    for program in [
        r#"import std.text.length
fn wrong(value Text) Bool
requires value == "雪" || value == "é"
ensures result
{ length(value) > 2 }
"#,
        r#"fn wrong(value Int) Int
requires value >= -100 && value <= 100
ensures result != 2
{ value * 2 }
"#,
        r#"fn wrong(value Int) Int
ensures value + 1 > value
{ 0 }
"#,
    ] {
        fs::write(&source, program).unwrap();
        let output = common::loom(&["check", source.to_str().unwrap()]);
        assert!(!output.status.success(), "unsound proof: {program}");
    }
    fs::write(
        &source,
        "fn identity(value Int) Int ensures result == value { value }",
    )
    .unwrap();
    success(
        &common::command(&["check", source.to_str().unwrap()])
            .env("PATH", "")
            .output()
            .unwrap(),
    );
    let output = common::command(&["check", "compiler/examples/smt_contracts"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("required proof needs Z3"));
}
