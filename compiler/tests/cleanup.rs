use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn lexical_cleanup_preserves_native_results_with_scope_only_registration() {
    let package = tempfile::tempdir().unwrap();
    let executable = common::executable(package.path(), "cleanup");
    success(
        &common::command(&[
            "build",
            common::root()
                .join("compiler/examples/cleanup")
                .to_str()
                .unwrap(),
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

    fs::write(
        package.path().join("main.loom"),
        "fn explicit() Int { var x = 3\ndefer { x = 9 }\nreturn x }\n\
         fn implicit() Int { var x = 4\ndefer { x = 8 }\nx }\n\
         fn nested() Int { var x = 1\n{ defer { x = x + 2 }\nx = 3 }\nx }\n\
         fn main() { assert explicit() == 3 && implicit() == 4 && nested() == 5 }",
    )
    .unwrap();
    let executable = common::executable(package.path(), "scalar-cleanup");
    let ir = package.path().join("cleanup.ll");
    success(
        &common::command(&[
            "build",
            package.path().to_str().unwrap(),
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
    assert!(ir.contains("loom_rt_cleanup_push") && ir.contains("loom_rt_cleanup_pop"));
    assert!(!ir.contains("roots_enter") && !ir.contains("executor"));
}

#[test]
fn cleanup_rejects_escaping_control_flow_and_keeps_required_proofs() {
    let package = tempfile::tempdir().unwrap();
    for text in [
        "fn main() { defer { 1 } }",
        "fn main() { defer { return } }",
        "fn main() { defer { comptime if true {} else { return } } }",
        "fn main() { defer { comptime if true {} else { defer {} } } }",
        "import std.result.Result\n\
         fn fail() Result[Int, Text] { Result.Err(\"error\") }\n\
         fn guarded() Result[Int, Text] {\n\
             defer { comptime if true {} else { discard fail()? } }\n\
             Result.Ok(1)\n\
         }\nfn main() { discard guarded() }",
        "fn main() { defer { discard later }\nlet later = 1\ndiscard later }",
        "fn unproved() Int ensures result == 1 { defer {}\n0 }\nfn main() {}",
        "fn cleanup() {}\n\
         fn unsupported(value Int) Int ensures result == value { defer { cleanup() }\nvalue }\n\
         fn main() {}",
    ] {
        fs::write(package.path().join("main.loom"), text).unwrap();
        let output = loom(&["check", package.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "accepted {text}: {output:?}");
        assert!(!output.stderr.is_empty());
    }
}
