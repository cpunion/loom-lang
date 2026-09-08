use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn proved_body_calls_keep_native_calls_argument_order_and_overflow_faults() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.loom"),
        r#"import std.process.arguments
import std.list.length

fn identity(value Int) Int ensures result == value { value }
fn forward(value Int) Int ensures result == value { identity(value) }
fn first(left Int, right Int) Int ensures result == left { left }
fn snapshot() Int ensures result == 3 {
    var value = 1
    let before = first(value, { value = 2
        value })
    before + value
}
fn next(value Int) Int { value + 1 }
fn advanced(value Int) Int ensures result > value { next(value) }
fn main() {
    assert forward(7) == 7
    assert snapshot() == 3
    if length(arguments()) > 1 {
        discard advanced(9223372036854775807)
    } else { assert advanced(7) == 8 }
}"#,
    )
    .unwrap();
    let artifact = common::executable(directory.path(), "call-proofs");
    let ir_path = directory.path().join("call-proofs.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                directory.path().to_str().unwrap(),
                "--output",
                artifact.to_str().unwrap(),
                "--emit-ir",
                ir_path.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&Command::new(&artifact).output().unwrap());
        let overflow = Command::new(&artifact).arg("overflow").output().unwrap();
        assert_eq!(overflow.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&overflow.stderr).contains("overflow"));
        if level == "0" {
            let ir = fs::read_to_string(&ir_path).unwrap();
            assert!(
                ir.matches("call i64 @loom.fn.").count() >= 4,
                "proof expansion must not replace the original runtime calls"
            );
        }
    }
}
