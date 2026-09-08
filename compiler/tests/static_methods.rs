use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn static_methods_erase_constants_and_preserve_managed_dynamic_call_order() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"import std.list.new
import std.list.push
import std.list.length
import std.text.concat

concept Join {
    fn join(self Self, left Text, comptime separator Text, right Text,
        comptime action fn(Text) Text, comptime tag Int, comptime enabled Bool) Text
    fn unused(self Self, comptime value Int) Int { 91827364 }
}
record Joiner {
    prefix Text
    events List[Int]
}
impl Join for Joiner {
    fn join(self Joiner, left Text, comptime separator Text, right Text,
        comptime action fn(Text) Text, comptime tag Int, comptime enabled Bool) Text {
        push(self.events, tag)
        comptime if enabled {
            concat(self.prefix, concat(left, concat(separator, action(right))))
        } else { concat(left, right) }
    }
}
fn receiver(events List[Int]) dyn Join {
    push(events, 1)
    Joiner { prefix = concat("P", "") events = events }
}
fn argument(events List[Int], tag Int, value Text) Text {
    push(events, tag)
    concat(value, "")
}
fn decorate(value Text) Text { concat(value, "!") }
fn identity(value Text) Text { value }

record IntHook { pick fn(Int) Int }
record BoolHook { pick fn(Bool) Bool }
concept Flag { fn pick(self Self, comptime value Bool) Bool { value } }
concept Number { fn pick(self Self, comptime value Int) Int { value } }
impl Flag for IntHook {}
impl Number for BoolHook {}
fn next(value Int) Int { value + 1 }
fn flip(value Bool) Bool { !value }
fn number(events List[Int]) Int { push(events, 6)
    7 }
fn flag(events List[Int]) Bool { push(events, 7)
    false }

type Positive = Int where self > 0
record CheckedHook { pick fn(Positive) Int }
impl Flag for CheckedHook {}
fn positive(value Positive) Int { value }
fn checked_argument(value Int) Int {
    assert value > 0
    let hook = CheckedHook { pick = positive }
    // Probing the competing static Bool method must retain the caller's fact.
    hook.pick(Positive(value))
}

fn main() {
    let events = new[Int]()
    assert receiver(events).join(argument(events, 2, "L"), "|",
        argument(events, 3, "R"), decorate, 4, true) == "PL|R!"
    assert receiver(events).join(argument(events, 2, "L"), "|",
        argument(events, 3, "R"), decorate, 4, true) == "PL|R!"
    assert receiver(events).join(argument(events, 2, "L"), "/",
        argument(events, 3, "R"), identity, 5, true) == "PL/R"
    let int_hook = IntHook { pick = next }
    let bool_hook = BoolHook { pick = flip }
    assert int_hook.pick(number(events)) == 8 && int_hook.pick(true)
    assert bool_hook.pick(flag(events)) && bool_hook.pick(9) == 9
    assert length(events) == 14
    var group = 0
    while group < 3 {
        let start = group * 4
        assert events[start] == 1 && events[start + 1] == 2 && events[start + 2] == 3
        assert events[start + 3] == if group < 2 { 4 } else { 5 }
        group = group + 1
    }
    assert events[12] == 6 && events[13] == 7
    assert checked_argument(8) == 8
}"#,
    )
    .unwrap();
    let example = common::root().join("compiler/examples/comptime_parameters");
    let artifact = common::executable(package.path(), "static-methods");
    let example_artifact = common::executable(package.path(), "comptime-parameters");
    let ir_path = package.path().join("static-methods.ll");
    for level in ["0", "2"] {
        // The maintained example covers default forwarding, generic methods,
        // scalar/function constants and CTFE; this fixture adds moving-GC roots.
        success(
            &common::command(&[
                "build",
                example.to_str().unwrap(),
                "--output",
                example_artifact.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(
            &Command::new(&example_artifact)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        success(
            &common::command(&[
                "build",
                package.path().to_str().unwrap(),
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
        let ir = fs::read_to_string(&ir_path).unwrap();
        assert!(
            !ir.contains("91827364"),
            "unused static method reached LLVM"
        );
        if level == "0" {
            let thunks: Vec<_> = ir
                .lines()
                .filter(|line| {
                    line.starts_with("define ")
                        && line.contains("@loom.witness.")
                        && line.contains(".method.")
                })
                .collect();
            assert_eq!(
                thunks.len(),
                2,
                "repeated constants need one slot: {thunks:?}"
            );
            for thunk in thunks {
                let params = thunk.split_once('(').unwrap().1.split(')').next().unwrap();
                let params: Vec<_> = params.split(',').collect();
                assert_eq!(params.len(), 3, "static values leaked into ABI: {thunk}");
                assert!(
                    params
                        .iter()
                        .all(|param| param.trim_start().starts_with("ptr ")),
                    "expected receiver and two managed Text arguments: {thunk}"
                );
            }
        }
    }
}
