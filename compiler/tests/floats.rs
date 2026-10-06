use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn floats_use_native_aggregates_and_scalar_programs_need_no_loom_runtime() {
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("main.loom"),
        r#"
type Money = Float where self >= 0.0

fn widen(value Money) Float
ensures result >= 0.0
{
    value
}

fn remainder(a Float, b Float) Float {
    a % b
}

fn main() {
    assert remainder(5.5, 2.0) == 1.5
    assert widen(Money(10.0)) == 10.0
}
"#,
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
    assert!(!ir.contains("fcmp oge double"));

    let example = common::root().join("compiler/examples/floats");
    let executable = common::executable(source.path(), "aggregates");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                example.to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
            ])
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
}

#[test]
fn float_mixing_and_unproved_contracts_reject_in_source() {
    let source = tempfile::tempdir().unwrap();
    for text in [
        "fn main() { discard 1 + 1.0 }",
        "fn main() { let value Float = 1 }",
        "fn main() { let value Int = 1.0 }",
        "fn main() { discard 1.0 && 2.0 }",
        "fn main() { discard 1.e2 }",
        "fn bad(value Float) Float ensures result == value { value }",
        "fn bad() Bool ensures result { 0.0 / 0.0 == 0.0 / 0.0 }",
        r#"
fn zero() Float
ensures result == 0.0 {
    -0.0
}
fn reciprocal() Float
ensures result > 0.0 {
    1.0 / zero()
}
"#,
        r#"
fn pair() (Float, Int)
ensures result.0 == 0.0 && result.1 == 1 {
    (-0.0, 1)
}
fn reciprocal() Float
ensures result > 0.0 {
    1.0 / pair().0
}
"#,
        r#"
fn unsafe(a Float, b Float) Bool
requires a <= b && b <= a
ensures result {
    1.0 / a == 1.0 / b
}
"#,
        r#"
fn unsafe(value Float) Bool
requires !(value < 1.0)
ensures result {
    value >= 1.0
}
"#,
    ] {
        fs::write(source.path().join("main.loom"), text).unwrap();
        let output = loom(&["check", source.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{text}: {output:?}");
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn float_method_normal_return_proofs_retain_runtime_entry_checks() {
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("main.loom"),
        r#"
concept Positive {
    fn keep(self Self, value Float) Float
    requires value > 0.0
    ensures result > 0.0 {
        value
    }
}

impl Positive for Bool {}

fn guarded(source dyn Positive, value Float) Float
ensures result > 0.0 {
    source.keep(value)
}

fn main() {
    let source dyn Positive = true
    discard guarded(source, 0.0 / 0.0)
}
"#,
    )
    .unwrap();
    let executable = common::executable(source.path(), "guarded");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                source.path().to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = Command::new(&executable).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("precondition failed"));
    }
}

#[test]
fn float_order_proofs_do_not_remove_non_nan_entry_checks() {
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("main.loom"),
        r#"
import std.float.is_nan

fn bounded(value Float, lower Float, upper Float) Float
requires !is_nan(value) && lower <= upper
ensures result >= lower && result <= upper {
    if value < lower {
        lower
    } else {
        if value > upper {
            upper
        } else {
            value
        }
    }
}

fn main() {
    discard bounded(0.0 / 0.0, 1.0, 2.0)
}
"#,
    )
    .unwrap();
    let executable = common::executable(source.path(), "unordered");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                source.path().to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = Command::new(&executable).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("precondition failed"));
    }
}
