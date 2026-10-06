use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn ieee_arithmetic_contracts_compile_and_revalidate_real_edits() {
    let package = tempfile::tempdir().unwrap();
    let path = package.path().to_str().unwrap();
    let source = package.path().join("main.loom");
    let program = format!(
        "{}\nfn main() {{\n    float_solver_cases()\n}}\ntest fn arithmetic() {{\n    main()\n}}\n",
        include_str!("../examples/smt_contracts/floats.loom")
    );
    let cache = package.path().join("cache");
    let check = || common::loom(&["check", path, "--frontend-cache", cache.to_str().unwrap()]);
    fs::write(&source, &program).unwrap();
    success(&check());
    success(&check());
    fs::write(&source, program.replace("    -value\n}", "    value\n}")).unwrap();
    let changed = check();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("not proved"));
    fs::write(&source, &program).unwrap();
    success(&check());
    let artifact = common::executable(package.path(), "float-smt");
    for level in ["0", "2"] {
        success(
            &common::command(&["test", path])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &common::command(&["build", path, "--output", artifact.to_str().unwrap()])
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
    }
    success(&common::loom(&["run", path]));
}

#[test]
fn float_solver_does_not_invent_real_algebra_or_a_remainder_theory() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    for program in [
        "fn bad(value Float) Float ensures result >= 0.0 {\n    if value < 0.0 { -value } else { value }\n}",
        "fn bad(value Float) Float ensures result == 0.0 {\n    value - value\n}",
        "fn bad(value Float) Float requires value > 0.0 ensures result > 0.0 {\n    value / 2.0\n}",
        "fn bad(value Float) Float requires value == 5e-324 ensures result > 0.0 {\n    value / 2.0\n}",
        "fn bad(value Float) Float requires value > 0.0 ensures result > value {\n    value + 1.0\n}",
        "fn bad(a Float, b Float, c Float) Bool ensures result {\n    (a + b) + c == a + (b + c)\n}",
        "fn bad(value Float) Float requires value == 3.0 ensures result == -1.0 {\n    value % 2.0\n}",
        "fn bad(a Float, b Float) Bool requires a == b ensures result {\n    1.0 / a == 1.0 / b\n}",
    ] {
        fs::write(&source, program).unwrap();
        let output = common::loom(&["check", package.path().to_str().unwrap()]);
        assert!(!output.status.success(), "unsound Float proof: {program}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("proved") || error.contains("SMT"), "{error}");
    }
}
