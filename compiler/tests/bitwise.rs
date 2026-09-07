use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn bitwise_operations_use_native_instructions_and_match_compile_time_results() {
    let source = common::root().join("compiler/examples/bitwise");
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "bitwise");
    let ir = temporary.path().join("bitwise.ll");
    success(&loom(&["check", source.to_str().unwrap()]));
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                source.to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&Command::new(&executable).output().unwrap());
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(!ir.contains("loom_rt_"));
        if level == "0" {
            for instruction in ["and i64", "or i64", "xor i64", "shl i64", "ashr i64"] {
                assert!(ir.contains(instruction), "missing {instruction}");
            }
        }
    }
}

#[test]
fn shifts_fault_before_llvm_undefined_behavior_and_bitwise_operands_are_eager() {
    let source = tempfile::tempdir().unwrap();
    let executable = common::executable(source.path(), "fault");
    for (body, message) in [
        ("discard 1 << -1", "shift"),
        ("discard 1 >> 64", "shift"),
        ("discard 0 & (1 / 0)", "zero"),
    ] {
        fs::write(
            source.path().join("main.loom"),
            format!("fn main() {{ {body} }}"),
        )
        .unwrap();
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
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(message),
                "{output:?}"
            );
        }
    }
}
