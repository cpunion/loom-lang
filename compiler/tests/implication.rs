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
