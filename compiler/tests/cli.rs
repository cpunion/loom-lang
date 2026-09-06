use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn test_no_run_builds_a_real_test_binary_without_executing_tests() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        "fn private_value() Int { 42 }\ntest fn deliberately_fails() { assert private_value() == 0 }",
    )
    .unwrap();
    let executable = common::executable(package.path(), "compiled-tests");
    let output = loom(&[
        "test",
        package.path().to_str().unwrap(),
        "--no-run",
        "--output",
        executable.to_str().unwrap(),
    ]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("compiled "));
    assert!(String::from_utf8_lossy(&output.stdout).contains(executable.to_str().unwrap()));
    assert!(executable.is_file());
    assert!(!Command::new(&executable).output().unwrap().status.success());
    assert!(
        !loom(&["test", package.path().to_str().unwrap()])
            .status
            .success()
    );

    // No tests is not a request to produce a runnable main or a stale artifact.
    fs::write(
        package.path().join("main.loom"),
        "fn main() { assert false }",
    )
    .unwrap();
    let absent = common::executable(package.path(), "no-tests");
    let output = loom(&[
        "test",
        package.path().to_str().unwrap(),
        "--no-run",
        "--output",
        absent.to_str().unwrap(),
    ]);
    success(&output);
    assert_eq!(output.stdout, b"0 tests\n");
    assert!(!absent.exists());
}
