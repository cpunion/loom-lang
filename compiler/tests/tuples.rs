use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn tuple_values_run_natively_and_keep_managed_fields_alive() {
    let example = common::root().join("compiler/examples/tuples");
    success(&loom(&["check", example.to_str().unwrap()]));
    success(&loom(&["test", example.to_str().unwrap()]));
    success(&loom(&["run", example.to_str().unwrap()]));
    let output = tempfile::tempdir().unwrap();
    let executable = common::executable(output.path(), "tuples");
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
fn tuple_errors_are_source_diagnostics_not_native_failures() {
    let source = tempfile::tempdir().unwrap();
    for text in [
        "fn main() { let a, b = (1,) }",
        "fn main() { let a, b = 1 }",
        "fn main() { let a, a = (1, 2) }",
        "fn main() { discard (1, true).2 }",
        "fn main() { discard (1, true).999999999999999999999999999 }",
        "fn main() { let value (Int, Bool) = (true, 1) }",
        "record Cycle { next (Int, Cycle) }",
        "fn main() { discard () }",
    ] {
        fs::write(source.path().join("main.loom"), text).unwrap();
        let output = loom(&["check", source.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{text}: {output:?}");
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn scalar_tuple_expansion_has_no_runtime_container_or_scheduler() {
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("main.loom"),
        r#"
fn sum(a Int, b Int) Int {
    a + b
}
fn expanded(values (Int, Int)) Int {
    sum(values...)
}
fn main() {
    assert expanded((20, 22)) == 42
    assert (20, (true, 22)) == (20, (true, 22))
}
"#,
    )
    .unwrap();
    let executable = common::executable(source.path(), "expanded");
    let ir = source.path().join("expanded.ll");
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
    let ir = fs::read_to_string(ir).unwrap();
    assert!(!ir.contains("call ptr @loom_"));
    assert!(!ir.contains("@loom_task_"));
    success(&Command::new(executable).output().unwrap());
}

#[test]
fn tuple_equality_eagerly_evaluates_a_faulting_right_operand() {
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("main.loom"),
        r#"
fn main() {
    discard (0, true) == {
        assert false
        (1, true)
    }
}
"#,
    )
    .unwrap();
    let executable = common::executable(source.path(), "equality");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", source.path().to_str().unwrap()])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        let output = Command::new(&executable).output().unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("assertion failed"));
    }
}

#[test]
fn empty_static_expansions_preserve_fault_order_and_lexical_cleanup() {
    let source = tempfile::tempdir().unwrap();
    let program = r#"
import std.io.write_text
import std.int.to_text

fn gap[Ts...](log List[Int], digit Int, fail Bool, values Ts...) (Ts...) {
    log[0] = log[0] * 10 + digit
    assert !fail
    values
}

fn only(comptime value Int) Int {
    assert value < 0
    value
}

fn main() {
    let log = [0]
    defer {
        discard write_text(to_text(log[0]))
    }
    discard only(gap(log, 1, GAP_FAIL)..., 42, gap(log, 2, false)...)
}
"#;
    let executable = common::executable(source.path(), "empty-static-fault");
    for fail_gap in [false, true] {
        fs::write(
            source.path().join("main.loom"),
            program.replace("GAP_FAIL", if fail_gap { "true" } else { "false" }),
        )
        .unwrap();
        for level in ["0", "2"] {
            success(
                &common::command(&["build", source.path().to_str().unwrap()])
                    .arg("--output")
                    .arg(&executable)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            let output = Command::new(&executable)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap();
            assert!(!output.status.success(), "{output:?}");
            assert_eq!(
                output.stdout,
                if fail_gap { b"1".as_slice() } else { b"12" }
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("assertion failed"));
        }
    }
}
