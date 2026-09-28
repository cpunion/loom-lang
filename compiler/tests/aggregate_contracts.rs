use std::{fs, process::Command};

mod common;
use common::{loom, success};

#[test]
fn aggregate_contract_calls_run_natively_without_proof_only_roots() {
    let package = "compiler/examples/aggregate_contracts";
    for command in ["check", "test", "run"] {
        success(&loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "aggregate-contracts");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &Command::new(&executable)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }

    let scalar = temporary.path().join("scalar");
    fs::create_dir(&scalar).unwrap();
    fs::write(
        scalar.join("main.loom"),
        r#"
record Pair {
    first Int
    second Int
}
fn proof_only(value Pair) Bool {
    let marker = 91827365
    marker > 0 && value.first == value.first
}
fn identity(value Pair) Pair
ensures proof_only(result)
ensures result.first == value.first
ensures result.second == value.second
{
    value
}
fn forward(value Pair) Pair
ensures result.first == value.first
ensures result.second == value.second
{
    identity(identity(value))
}
fn main() {
    let value = forward(Pair { first = 3, second = 7 })
    assert value.first == 3 && value.second == 7
}
"#,
    )
    .unwrap();
    let ir = temporary.path().join("scalar.ll");
    success(
        &common::command(&["build"])
            .arg(&scalar)
            .arg("--output")
            .arg(&executable)
            .arg("--emit-ir")
            .arg(&ir)
            .env("LOOM_OPT_LEVEL", "0")
            .output()
            .unwrap(),
    );
    let source = fs::read_to_string(ir).unwrap();
    assert!(
        !source.contains("91827365"),
        "proof-only helper became a runtime root"
    );
    assert!(
        !source.contains("@loom_rt_"),
        "scalar record calls require runtime support"
    );
    success(&Command::new(executable).output().unwrap());
}
