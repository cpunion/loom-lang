use std::{fs, process::Command};
mod common;
use common::success;

const RESTORE: &str = r#"import std.list.get
import std.list.set
import std.list.length

fn restored(values List[Int], observed Int, changed Int)
requires observed >= 0 && observed < length(values)
requires changed >= 0 && changed < length(values)
ensures get(values, observed) == old(get(values, observed))
{
    let saved = get(values, changed)
    set(values, changed, 7)
    set(values, changed, saved)
}
"#;

#[test]
fn heap_versions_reject_wrong_writes_alias_interference_and_undefined_reads() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    for program in [
        RESTORE.replace("set(values, changed, saved)", "set(values, changed, 7)"),
        RESTORE
            .replace("changed Int)", "changed Int, other List[Int])")
            .replace(
                "ensures get",
                "requires changed < length(other)\nensures get",
            )
            .replace(
                "set(values, changed, saved)",
                "set(values, changed, saved)\n    set(other, changed, 9)",
            ),
        format!(
            "{}\nfn change(values List[Int], index Int) {{\n    set(values, index, 9)\n}}\n",
            RESTORE.replace(
                "set(values, changed, saved)",
                "set(values, changed, saved)\n    change(values, changed)",
            )
        ),
        RESTORE.replace("requires observed >= 0 && observed < length(values)\n", ""),
    ] {
        fs::write(&source, &program).unwrap();
        let output = common::loom(&["check", package.path().to_str().unwrap()]);
        assert!(!output.status.success(), "unsound heap proof: {program}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("postcondition") || error.contains("List element proof"),
            "unexpected failure: {error}\n{program}"
        );
    }
}

#[test]
fn cached_heap_proofs_revalidate_edits_and_worker_interference() {
    let package = tempfile::tempdir().unwrap();
    let cache = package.path().join("cache");
    let source = package.path().join("restore.loom");
    let main = package.path().join("main.loom");
    fs::write(&source, RESTORE).unwrap();
    fs::write(&main, "fn main() {\n    restored([1], 0, 0)\n}\n").unwrap();
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    success(&check());
    success(&check());
    fs::write(
        &source,
        RESTORE.replace("set(values, changed, saved)", "set(values, changed, 7)"),
    )
    .unwrap();
    let wrong = check();
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("postcondition"));
    fs::write(&source, RESTORE).unwrap();
    success(&check());
    fs::write(
        &main,
        r#"import std.task.worker.run

async fn main() {
    discard run(fn() Int {
            restored([1], 0, 0)
            1
        }).await
}
"#,
    )
    .unwrap();
    let shared = check();
    assert!(!shared.status.success());
    let error = String::from_utf8_lossy(&shared.stderr);
    assert!(error.contains("interference-safe"), "{error}");
}

#[test]
fn solver_contracts_keep_editor_concept_navigation() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    let text = r#"concept Read {
    fn read(self Self) Int
}

fn read[T Read](value T) Int {
    value.read()
}

fn square(value Int) Int
ensures result >= 0
{
    value * value
}
"#;
    fs::write(&source, text).unwrap();
    let output = common::loom(&[
        "editor-query",
        temporary.path().to_str().unwrap(),
        "--at",
        source.to_str().unwrap(),
        &text.find("Read]").unwrap().to_string(),
    ]);
    success(&output);
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("\"diagnostics\":[]"), "{report}");
    assert!(report.contains("\"start\":8,\"end\":12"), "{report}");
    assert!(!report.contains("\"definitions\":[]"), "{report}");
}

#[test]
fn solver_contracts_check_and_compile_without_a_runtime_solver() {
    let package = "compiler/examples/smt_contracts";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "smt-contracts");
    let ir = temporary.path().join("app.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .arg("--emit-ir")
                .arg(&ir)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        let emitted = fs::read_to_string(&ir).unwrap();
        assert!(!emitted.contains("process_capture"));
        success(
            &Command::new(&executable)
                .env("PATH", "")
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn solver_refutes_counterexamples_and_does_not_rescue_undefined_contracts() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    for program in [
        r#"import std.text.length
fn wrong(value Text) Bool
requires value == "雪" || value == "é"
ensures result
{ length(value) > 2 }
"#,
        r#"fn wrong(value Int) Int
requires value >= -100 && value <= 100
ensures result != 2
{ value * 2 }
"#,
        r#"fn wrong(value Int) Int
ensures value + 1 > value
{ 0 }
"#,
        r#"import std.text.length
fn wrong(value Text, number Int) Bool
requires length(value) == 0
ensures value != "" || number + 1 > number
{ true }
"#,
        r#"import std.text.length
fn either(first Bool, second Bool) Bool { first || second }
fn wrong(value Text, number Int) Bool
requires length(value) == 0
ensures either(value == "", number + 1 > number)
{ true }
"#,
        r#"fn wrong(value Int) Int
ensures result > 0
{ value * value }
"#,
        r#"fn wrong(value Int) Bool
ensures value * value >= 0
{ true }
"#,
        r#"fn wrong(value Int) Int
requires value >= -100 && value <= 100
ensures result == value * value + 1
{ value * value }
"#,
    ] {
        fs::write(&source, program).unwrap();
        let output = common::loom(&["check", temporary.path().to_str().unwrap()]);
        assert!(!output.status.success(), "unsound proof: {program}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("postcondition"),
            "unexpected failure: {error}"
        );
    }
    fs::write(
        &source,
        r#"fn identity(value Int) Int ensures result == value { value }
fn bounded(value Int) Int
requires value >= -3 && value <= 4
ensures result <= 16
{
    value * value
}
"#,
    )
    .unwrap();
    success(
        &common::command(&["check", temporary.path().to_str().unwrap()])
            .env("PATH", "")
            .output()
            .unwrap(),
    );
    let output = common::command(&["check", "compiler/examples/smt_contracts"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("required proof needs Z3"));
}

#[test]
fn solver_refinement_proofs_erase_checks_but_not_eager_faults() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    fs::write(
        &source,
        r#"import std.list.get
import std.list.set

type Small = Int where self >= -10 && self <= 10

fn square_nonnegative(value Int) Bool {
    value * value >= 0
}

type SafeSquare = Int where square_nonnegative(self)

fn observed(counter List[Int]) Small {
    set(counter, 0, get(counter, 0) + 1)
    Small(3)
}

fn weaken(value Small) SafeSquare {
    SafeSquare(value)
}

fn main() {
    let counter = [0]
    assert weaken(observed(counter)) == 3
    assert get(counter, 0) == 1
}
"#,
    )
    .unwrap();
    let executable = common::executable(temporary.path(), "refinements");
    let ir = temporary.path().join("refinements.ll");
    success(
        &common::command(&["build", temporary.path().to_str().unwrap()])
            .arg("--output")
            .arg(&executable)
            .arg("--emit-ir")
            .arg(&ir)
            .env("LOOM_OPT_LEVEL", "0")
            .output()
            .unwrap(),
    );
    assert!(
        !fs::read_to_string(&ir).unwrap().contains("llvm.smul"),
        "proved predicate must disappear before LLVM optimization"
    );
    success(&Command::new(&executable).output().unwrap());
    for program in [
        r#"type UnitSquare = Int where self * self == 1
type Positive = Int where self > 0
fn wrong(value UnitSquare) Positive {
    Positive(value)
}
"#,
        r#"fn square_nonnegative(value Int) Bool {
    value * value >= 0
}
type SafeSquare = Int where square_nonnegative(self)
fn wrong(value Int) SafeSquare {
    SafeSquare(value)
}
"#,
        r#"fn either(first Bool, second Bool) Bool {
    first || second
}
type SafeNext = Int where either(true, self + 1 > self)
fn wrong(value Int) SafeNext {
    SafeNext(value)
}
"#,
    ] {
        fs::write(&source, program).unwrap();
        let output = common::loom(&["check", temporary.path().to_str().unwrap()]);
        assert!(!output.status.success(), "unsound refinement: {program}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("Result["), "unexpected failure: {error}");
    }
}
