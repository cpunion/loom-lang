use std::{fs, process::Command};
mod common;
use common::success;

fn build(text: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("main.loom"), text).unwrap();
    let executable = common::executable(directory.path(), "implication");
    let ir = directory.path().join("implication.ll");
    success(
        &common::command(&[
            "build",
            directory.path().to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    (directory, executable, fs::read_to_string(ir).unwrap())
}

#[test]
fn stronger_int_refinements_need_no_second_check_before_llvm_optimization() {
    let (_directory, executable, ir) = build(
        "type Positive = Int where self > 0\n\
         type NonNegative = Int where self >= 0\n\
         fn wider(value Positive) NonNegative { NonNegative(value) }\n\
         fn main() { assert wider(Positive(7)) == 7 }",
    );
    success(&Command::new(executable).output().unwrap());
    assert!(!ir.contains("icmp sge") && !ir.contains("icmp sgt"));
    assert!(!ir.contains("loom_rt_"));
}

#[test]
fn float_conjunction_weakening_reuses_checks_without_ieee_algebra() {
    let (_directory, executable, ir) = build(
        r#"
type Bounded = Float where self >= 0.0 && self <= 100.0
type NonNegative = Float where self >= 0.0
type Reordered = Float where self <= 100.0 && self >= 0.0
type SelectedNaN = Float where self != self && self != 0.0
type NotANumber = Float where self != self
fn wider(value Bounded) NonNegative { NonNegative(value) }
fn reordered(value Bounded) Reordered { Reordered(value) }
fn keep_nan(value SelectedNaN) NotANumber { NotANumber(value) }
fn main() {
    assert wider(Bounded(12.5)) == 12.5
    assert reordered(Bounded(100.0)) == 100.0
    let value = keep_nan(SelectedNaN(0.0 / 0.0))
    assert value != value
}
"#,
    );
    success(&Command::new(executable).output().unwrap());
    assert!(!ir.contains("fcmp oge") && !ir.contains("fcmp ole"));
    assert!(!ir.contains("loom_rt_"));
}

#[test]
fn checked_narrowing_remains_and_widening_evaluates_its_input_once() {
    let (_directory, executable, ir) = build(
        "import std.result.Result\nimport std.result.ConstraintError\n\
         import std.list.new\nimport std.list.push\nimport std.list.get\nimport std.list.set\n\
         type Positive = Int where self > 0\n\
         type NonNegative = Int where self >= 0\n\
         fn source(count List[Int]) Positive { set(count, 0, get(count, 0) + 1)\nPositive(5) }\n\
         fn wider(count List[Int]) NonNegative { NonNegative(source(count)) }\n\
         fn narrower(value NonNegative) Result[Positive, ConstraintError] { Positive(value) }\n\
         fn main() {\n\
         let count = new[Int]()\npush(count, 0)\n\
         assert wider(count) == 5\nassert get(count, 0) == 1\n\
         match narrower(NonNegative(0)) { Result.Ok(_) => { assert false }\nResult.Err(_) => {} }\n\
         match narrower(NonNegative(3)) { Result.Ok(value) => { assert value == 3 }\nResult.Err(_) => { assert false } }\n}",
    );
    success(
        &Command::new(executable)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );
    assert!(ir.contains("icmp sgt i64"));
}

#[test]
fn mathematical_truth_does_not_remove_a_potentially_overflowing_guard() {
    let (_directory, executable, ir) = build(
        "import std.result.Result\nimport std.result.ConstraintError\n\
         import std.process.arguments\nimport std.list.length\n\
         type Whole = Int where true\n\
         type Increment = Int where self + 1 > self\n\
         fn checked(value Whole) Result[Increment, ConstraintError] { Increment(value) }\n\
         fn main() {\n\
         let value = if length(arguments()) > 1 { Whole(9223372036854775807) } else { Whole(41) }\n\
         match checked(value) { Result.Ok(found) => { assert found == 41 }\nResult.Err(_) => { assert false } }\n}",
    );
    success(&Command::new(&executable).output().unwrap());
    assert!(ir.contains("llvm.sadd.with.overflow.i64"));
    let overflow = Command::new(executable).arg("overflow").output().unwrap();
    assert_eq!(overflow.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&overflow.stderr).contains("overflow"));
}

#[test]
fn immutable_flow_facts_remove_only_repeated_checks_at_o0() {
    let (_directory, executable, ir) = build(
        r#"
import std.list.new
import std.list.push
import std.list.get
import std.list.set
type Positive = Int where self > 0
fn required(value Int) Positive requires value > 0 { Positive(value) }
fn branch(value Int) Positive {
    if value > 0 { Positive(value) } else { Positive(1) }
}
fn source(count List[Int]) Int {
    set(count, 0, get(count, 0) + 1)
    7
}
fn asserted(count List[Int]) Positive {
    let value = source(count)
    assert value > 0
    defer { discard Positive(value) }
    Positive(value)
}
fn main() {
    let count = new[Int]()
    push(count, 0)
    assert required(5) == 5
    assert branch(8) == 8
    assert branch(0) == 1
    assert asserted(count) == 7
    assert get(count, 0) == 1
}
"#,
    );
    success(
        &Command::new(executable)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );
    // One precondition, one branch condition and one assertion. Constructors,
    // including the deferred one, add no comparison before LLVM optimization.
    assert_eq!(ir.matches("icmp sgt i64").count(), 3);
}

#[test]
fn flow_boundaries_keep_nan_and_mutable_cleanup_checks() {
    let (_directory, executable, ir) = build(
        r#"
import std.result.Result
import std.result.ConstraintError
import std.list.new
import std.list.push
import std.list.get
import std.list.set
type Positive = Int where self > 0
type NonPositive = Float where self <= 0.0
type NotPositive = Float where !(self > 0.0)
fn floating(value Float) Result[NonPositive, ConstraintError] {
    assert !(value > 0.0)
    NonPositive(value)
}
fn exact(value Float) NotPositive {
    if value > 0.0 { NotPositive(0.0) } else { NotPositive(value) }
}
fn cleanup(count List[Int], value Int) {
    var current = value
    assert current > 0
    defer {
        match Positive(current) {
            Result.Ok(_) => { set(count, 0, 1) }
            Result.Err(_) => { set(count, 0, 2) }
        }
    }
    current = 0
}
fn main() {
    let nan = 0.0 / 0.0
    match floating(nan) {
        Result.Ok(_) => { assert false }
        Result.Err(_) => {}
    }
    match floating(0.0) {
        Result.Ok(value) => { assert value == 0.0 }
        Result.Err(_) => { assert false }
    }
    let preserved = exact(nan)
    assert preserved != preserved
    let count = new[Int]()
    push(count, 0)
    cleanup(count, 1)
    assert get(count, 0) == 2
}
"#,
    );
    success(
        &Command::new(executable)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );
    assert_eq!(ir.matches("fcmp ole double").count(), 1);
    assert_eq!(ir.matches("icmp sgt i64").count(), 2);
}
