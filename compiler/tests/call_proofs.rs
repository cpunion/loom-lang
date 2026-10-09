use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn concept_contracts_compose_without_emitting_abstract_proof_bodies() {
    let package = "compiler/examples/concept_contracts";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let directory = tempfile::tempdir().unwrap();
    let artifact = common::executable(directory.path(), "concept-contracts");
    let ir = directory.path().join("concepts.ll");
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
        success(
            &Command::new(&artifact)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        let llvm = fs::read_to_string(&ir).unwrap();
        for absent in ["executor", "proof", "universal"] {
            assert!(
                !llvm.contains(absent),
                "private proof machinery leaked: {absent}"
            );
        }
        if level == "0" {
            assert!(llvm.matches("call i64 @loom.fn.").count() >= 4);
            assert!(
                llvm.contains("call i64 %"),
                "dyn dispatch was replaced by a guessed witness"
            );
        }
    }
}

#[test]
fn relational_contracts_share_the_prover_and_preserve_real_faults() {
    let package = "compiler/examples/relational_contracts";
    success(&common::loom(&["check", package]));
    success(&common::loom(&["test", package]));
    success(&common::loom(&["run", package]));
    let directory = tempfile::tempdir().unwrap();
    let artifact = common::executable(directory.path(), "relational-contracts");
    let ir_path = directory.path().join("contracts.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package,
                "--output",
                artifact.to_str().unwrap(),
                "--emit-ir",
                ir_path.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(
            &Command::new(&artifact)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        let entry = Command::new(&artifact)
            .arg("invalid-entry")
            .output()
            .unwrap();
        assert_eq!(entry.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&entry.stderr).contains("precondition"));
        let overflow = Command::new(&artifact)
            .args(["body", "overflow"])
            .output()
            .unwrap();
        assert_eq!(overflow.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&overflow.stderr).contains("overflow"));
        if level == "0" {
            let ir = fs::read_to_string(&ir_path).unwrap();
            assert_eq!(
                ir.matches("icmp sgt i64").count(),
                6,
                "only the six source entry comparisons remain"
            );
            assert_eq!(
                ir.matches("call { i64, i1 } @llvm.sadd.with.overflow.i64")
                    .count(),
                2,
                "grow and argument collection retain their checked increments"
            );
        }
    }
}

#[test]
fn enum_predicates_compose_without_runtime_projections_and_recheck_edits() {
    let package = "compiler/examples/enum_predicates";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let directory = tempfile::tempdir().unwrap();
    let artifact = common::executable(directory.path(), "enum-predicates");
    let ir = directory.path().join("predicates.ll");
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
        success(&common::run_tasks(&artifact));
        let rejected = Command::new(&artifact)
            .arg("invalid-entry")
            .output()
            .unwrap();
        assert_eq!(rejected.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("precondition"));
        assert!(!fs::read_to_string(&ir).unwrap().contains("proof_enum_"));
    }
    let source = directory.path().join("main.loom");
    let original = include_str!("../examples/enum_predicates/main.loom");
    fs::write(&source, original).unwrap();
    let cache = directory.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            directory.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    success(&check());
    success(&check());
    for (before, after) in [
        ("Choice.Value(value)\n}", "Choice.Value(0)\n}"),
        (
            "Choice.Value(number) => number",
            "Choice.Value(number) => 0",
        ),
    ] {
        let changed = original.replace(before, after);
        assert_ne!(changed, original);
        fs::write(&source, changed).unwrap();
        let rejected = check();
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("postcondition"));
        fs::write(&source, original).unwrap();
        success(&check());
    }
}

#[test]
fn enum_list_columns_compose_native_snapshots_and_reject_false_cached_edits() {
    let package = "compiler/examples/enum_contents";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let directory = tempfile::tempdir().unwrap();
    let artifact = common::executable(directory.path(), "enum-contents");
    let ir = directory.path().join("contents.ll");
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
        success(&common::run_tasks(&artifact));
        assert!(!fs::read_to_string(&ir).unwrap().contains("proof_enum_"));
    }
    let original = include_str!("../examples/enum_contents/main.loom");
    let source = directory.path().join("main.loom");
    fs::write(&source, original).unwrap();
    let cache = directory.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            directory.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    success(&check());
    success(&check());
    for (before, after) in [
        ("values[changed] = saved", "discard saved"),
        (" && observed != changed", ""),
        ("Choice.Missing => true", "Choice.Missing => false"),
        (
            "type Positive = Int where self > 0",
            "type Positive = Int where self >= 0",
        ),
        (
            "type Label = Text where std.text.length(self) > 0",
            "type Label = Text where std.text.length(self) >= 0",
        ),
    ] {
        let changed = original.replace(before, after);
        assert_ne!(changed, original);
        fs::write(&source, changed).unwrap();
        let rejected = check();
        assert!(!rejected.status.success());
        let diagnostic = String::from_utf8_lossy(&rejected.stderr);
        assert!(
            diagnostic.contains("postcondition")
                || diagnostic.contains("required proof was not established"),
            "edit {before}: {diagnostic}"
        );
        fs::write(&source, original).unwrap();
        success(&check());
    }
    fs::write(
        &source,
        format!(
            "{original}\n{}",
            r#"
type Impossible = Int where self > 0 && self < 0
fn occupied(value Choice[Impossible]) Bool {
    match value {
        Choice.Value(_) => true
        Choice.Missing => false
    }
}
fn all_occupied(values List[Choice[Impossible]]) Bool {
    var index = 0
    while index < length(values) {
        if !occupied(values[index]) {
            return false
        }
        index = index + 1
    }
    true
}
fn wrong(values List[Choice[Impossible]]) List[Choice[Impossible]]
ensures all_occupied(result) {
    values
}
"#
        ),
    )
    .unwrap();
    let rejected = check();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("postcondition"));
}

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
