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
    fs::write(source.path().join("main.loom"),
        "fn sum(a Int, b Int) Int { a + b }\nfn expanded(values (Int, Int)) Int { sum(values...) }\nfn main() { assert expanded((20, 22)) == 42 }").unwrap();
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
