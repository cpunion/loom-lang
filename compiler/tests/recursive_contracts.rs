use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn recursive_contracts_compile_and_cached_groups_reject_bad_finite_cases() {
    let package = tempfile::tempdir().unwrap();
    let path = package.path().to_str().unwrap();
    let source = package.path().join("main.loom");
    let program = include_str!("../examples/recursive_contracts/main.loom");
    let cache = package.path().join("cache");
    let check = || common::loom(&["check", path, "--frontend-cache", cache.to_str().unwrap()]);
    fs::write(&source, program).unwrap();
    success(&check());
    success(&check());
    let odd_start = program.find("fn odd_count(").unwrap();
    let odd_end = program.find("fn generic_count[").unwrap();
    let wrong_odd = program[odd_start..odd_end].replacen("        0\n", "        1\n", 1);
    let bad_member = format!(
        "{}{}{}",
        &program[..odd_start],
        wrong_odd,
        &program[odd_end..]
    );
    for changed in [
        program.replacen("        0\n", "        1\n", 1),
        bad_member,
        program.replace("counted(value - 1) + 1", "counted(value - 1) + 2"),
        program.replace("ensures result == value", "ensures result == value + 1"),
        program.replacen("var count = 1", "var count = 0", 1),
    ] {
        assert_ne!(changed, program, "test edit did not apply");
        fs::write(&source, changed).unwrap();
        let output = check();
        assert!(
            !output.status.success(),
            "bad recursive member retained a proof"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("proved"), "{error}");
        assert!(!error.contains("staging depth"), "{error}");
        fs::write(&source, program).unwrap();
        success(&check());
    }
    let artifact = common::executable(package.path(), "recursive-contracts");
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
fn recursive_guarantees_do_not_make_contract_predicates_terminate() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"fn recursive(value Int) Bool
ensures result {
    recursive(value)
}
fn keep(value Int) Int
ensures recursive(result) {
    value
}
fn main() {
    discard keep(1)
}
"#,
    )
    .unwrap();
    let output = common::loom(&["check", package.path().to_str().unwrap()]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("recursive helper proof is unsupported"),
        "{error}"
    );
}
