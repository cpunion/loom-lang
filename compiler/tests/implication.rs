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

#[test]
fn scalar_helper_proofs_remove_calls_without_duplicating_input_evaluation() {
    let (_directory, executable, ir) = build(
        r#"
import std.list.new
import std.list.push
import std.list.get
import std.list.set
fn nonnegative(value Int) Bool { let minimum = 0
    return value >= minimum }
fn nested(value Int) Bool { nonnegative(value) }
fn not_positive(value Float) Bool { !(value > 0.0) }
type Positive = Int where self > 0
type Wide = Int where nested(self)
type Floating = Float where !(self > 0.0)
type Target = Float where not_positive(self)
fn input(count List[Int]) Positive {
    set(count, 0, get(count, 0) + 1)
    Positive(7)
}
fn widen(count List[Int]) Wide { Wide(input(count)) }
fn floating(value Floating) Target { Target(value) }
fn main() {
    let count = new[Int]()
    push(count, 0)
    assert widen(count) == 7
    assert get(count, 0) == 1
    let value = floating(Floating(0.0 / 0.0))
    assert value != value
}
"#,
    );
    success(
        &Command::new(executable)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );
    // These three predicates are the fixture's only Bool-returning functions.
    // None enters runtime reachability; List.get/set still legitimately emit
    // signed nonnegative-index comparisons in their separate bounds checks.
    assert!(!ir.contains("define internal i1 @loom.fn."));
    assert!(!ir.contains("fcmp ogt double"));
}

#[test]
fn helper_proofs_preserve_static_instance_faults_and_eager_arguments() {
    let (_directory, executable, ir) = build(
        r#"
import std.result.Result
import std.result.ConstraintError
import std.process.arguments
import std.list.length
import std.list.get
fn touched(value Int, comptime extra Int) Bool { let unused = value + extra
    true }
fn always(value Int) Bool { true }
fn admitted(value Int) Bool requires value > 0 { true }
fn either(a Bool, b Bool) Bool { a || b }
type Whole = Int where true
type Zero = Int where self == 0
type One = Int where touched(self, 1)
type Two = Int where touched(self, 2)
type Argument = Int where always(self + 1)
type Needs = Int where admitted(self)
type Eager = Int where either(self == 0, 10 / self > 0)
fn different(value One) Result[Two, ConstraintError] { Two(value) }
fn argument(value Whole) Result[Argument, ConstraintError] { Argument(value) }
fn requirement(value Whole) Result[Needs, ConstraintError] { Needs(value) }
fn eager(value Zero) Result[Eager, ConstraintError] { Eager(value) }
fn main() {
    let args = arguments()
    let mode = if length(args) > 1 { get(args, 1) } else { "ok" }
    if mode == "static" { discard different(One(9223372036854775806)) }
    else { if mode == "argument" { discard argument(Whole(9223372036854775807)) }
    else { if mode == "requires" { discard requirement(Whole(0)) }
    else { if mode == "eager" { discard eager(Zero(0)) }
    else { match different(One(40)) {
        Result.Ok(value) => { assert value == 40 }
        Result.Err(_) => { assert false }
    } } } } }
}
"#,
    );
    success(&Command::new(&executable).output().unwrap());
    assert!(ir.contains("llvm.sadd.with.overflow.i64") && ir.contains("sdiv i64"));
    for (mode, message) in [
        ("static", "overflow"),
        ("argument", "overflow"),
        ("requires", "precondition"),
        ("eager", "zero"),
    ] {
        let fault = Command::new(&executable).arg(mode).output().unwrap();
        assert_eq!(fault.status.code(), Some(1), "mode {mode}");
        assert!(String::from_utf8_lossy(&fault.stderr).contains(message));
    }
}
