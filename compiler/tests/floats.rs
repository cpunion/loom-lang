use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn floats_use_native_aggregates_and_scalar_programs_need_no_loom_runtime() {
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("main.loom"),
        "type Money = Float where self >= 0.0\nfn widen(value Money) Float { value }\nfn remainder(a Float, b Float) Float { a % b }\nfn main() { assert remainder(5.5, 2.0) == 1.5\nassert widen(Money(10.0)) == 10.0 }",
    )
    .unwrap();
    let executable = common::executable(source.path(), "scalar");
    let ir = source.path().join("scalar.ll");
    success(
        &common::command(&[
            "build",
            source.path().to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(&Command::new(executable).output().unwrap());
    let ir = fs::read_to_string(ir).unwrap();
    assert!(ir.contains("frem double"));
    assert!(!ir.contains("loom_rt_"));

    let example = common::root().join("compiler/examples/floats");
    let executable = common::executable(source.path(), "aggregates");
    success(
        &common::command(&[
            "build",
            example.to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(
        &Command::new(executable)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );
}

#[test]
fn float_mixing_and_unproved_contracts_reject_in_source() {
    let source = tempfile::tempdir().unwrap();
    for text in [
        "fn main() { discard 1 + 1.0 }",
        "fn main() { let value Float = 1 }",
        "fn main() { let value Int = 1.0 }",
        "fn main() { discard 1.0 && 2.0 }",
        "fn bad(value Float) Float ensures result == value { value }",
        "fn bad() Bool ensures result { 0.0 / 0.0 == 0.0 / 0.0 }",
    ] {
        fs::write(source.path().join("main.loom"), text).unwrap();
        let output = loom(&["check", source.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{text}: {output:?}");
        assert!(!output.stderr.is_empty());
    }
}
