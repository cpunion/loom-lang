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
fn loop_induction_rechecks_edits_and_rejects_counterexamples() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    let program = format!(
        "{}\n{}\n{}\n{}\nfn main() {{\n    float_loop_exercise()\n    transfer_exercise()\n    transfer_content_exercise()\n    opaque_exercise()\n}}\n",
        include_str!("../examples/loop_contracts/floats.loom"),
        include_str!("../examples/loop_contracts/transfers.loom"),
        include_str!("../examples/loop_contracts/contents.loom"),
        include_str!("../examples/loop_contracts/opaque.loom")
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
        program.replace("processed = processed + 1", "processed = processed + 2"),
        program.replace("output = append(output, item)", "output = append(output, 0)"),
        format!(
            "import std.list.set\n{}",
            program.replace(
                "rest = remaining\n                output = append(output, item)",
                "rest = remaining\n                if length(rest) > 0 { set(rest, 0, 0) }\n                output = append(output, item)",
            )
        ),
        program.replace(
            "output = append(output, value)",
            "output = append[T]([], value)",
        ),
        program.replace(
            "output = append(output, value)\n                continue",
            "if length(pending) == 0 { continue }\n                output = append(output, value)\n                continue",
        ),
        program.replace(
            "output = append(output, value)\n                continue",
            "output = append(output, value)\n                defer { discard take_last(output) }\n                continue",
        ),
        program.replace("current = current / 2.0", "current = -current"),
        program.replace("effect(state)", "effect(state)\n    push(fresh, 8)"),
        program.replace("let changed, value = next(current)", "let changed, value = next(current)\n        push(output, value)"),
        program.replace("current = current / 2.0", "current = 0.0 / 0.0"),
        r#"
import std.list.length
import std.list.transfer.take_last

fn invalid(left List[Int], right List[Int])
ensures length(right) == old(length(right)) {
    while length(left) > 0 {
        discard take_last(left)
    }
}
"#
        .to_owned(),
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
        assert!(!output.status.success(), "unsound loop: {changed}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("proved") || error.contains("SMT"), "{error}");
    }
    fs::write(&source, &program).unwrap();
    success(&check());
}
