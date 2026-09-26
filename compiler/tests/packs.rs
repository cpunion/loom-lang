use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn variadic_functions_run_with_native_values_compile_time_graphs_and_tasks() {
    let example = common::root().join("compiler/examples/variadics");
    let package = example.to_str().unwrap();
    for command in ["check", "test", "run"] {
        success(&loom(&[command, package]));
    }
    let output = tempfile::tempdir().unwrap();
    let executable = common::executable(output.path(), "packs");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package, "--output", executable.to_str().unwrap()])
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
fn static_pack_iteration_runs_in_an_independent_package() {
    let example = common::root().join("compiler/examples/pack_iteration");
    let package = example.to_str().unwrap();
    for command in ["check", "test", "run"] {
        success(&loom(&[command, package]));
    }
    let output = tempfile::tempdir().unwrap();
    let executable = common::executable(output.path(), "pack_iteration");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package, "--output", executable.to_str().unwrap()])
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
fn structural_tuple_settled_runs_at_o0_and_o2_with_gc_stress() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.loom"),
        r#"
import std.task.settled
import std.task.Outcome
import std.time.sleep_ms
import std.int.to_text

fn empty[Ts...](values Ts...) (Ts...) { values }
concept Show { fn show(self Self) Text }
impl Show for Int { fn show(self Int) Text { to_text(self) } }
impl Show for Bool { fn show(self Bool) Text { if self { "true" } else { "false" } } }
fn callbacks[Ts... Show](values (Ts...)) (fn() Text, fn() Text) {
    comptime map item in values { fn() Text { item.show() } }
}
fn map_once[Ts...](values (Ts...)) (Ts...) {
    var count = 0
    let mapped = comptime map item in values {
        count = count + 1
        item
    }
    assert count == 3
    mapped
}
async fn number(value Int) Int { sleep_ms(2).await
value }
async fn label() Text { "label" }
async fn nested() Task[Int] { number(9) }
async fn failure() Int { assert false
0 }
fn make() (Task[Int], Task[Text], Task[Task[Int]]) {
    (number(4), label(), nested())
}

async fn main() {
    let readers = callbacks((3, true))
    assert readers.0() == "3" && readers.1() == "true"
    let mapped = map_once((2, true, "mapped"))
    assert mapped.0 == 2 && mapped.1 && mapped.2 == "mapped"
    let none = settled(empty()).await
    discard none
    let one = settled((number(1),)).await
    match one.0 {
        Outcome.Completed(value) => { assert value == 1 }
        _ => { assert false }
    }
    let tasks = make()
    let a, b, c = settled(tasks).await
    match a {
        Outcome.Completed(value) => { assert value == 4 }
        _ => { assert false }
    }
    match b {
        Outcome.Completed(value) => { assert value == "label" }
        _ => { assert false }
    }
    match c {
        Outcome.Completed(task) => { assert task.await == 9 }
        _ => { assert false }
    }
    let first, second, third = settled((failure(), sleep_ms(0), label())).await
    match first {
        Outcome.Faulted(_) => {}
        _ => { assert false }
    }
    match second {
        Outcome.Completed(_) => {}
        _ => { assert false }
    }
    match third {
        Outcome.Completed(value) => { assert value == "label" }
        _ => { assert false }
    }
}
"#,
    )
    .unwrap();
    let package = directory.path().to_str().unwrap();
    success(&loom(&["check", package]));
    let executable = common::executable(directory.path(), "tuple_settled");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package, "--output", executable.to_str().unwrap()])
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
fn scalar_packs_do_not_introduce_runtime_storage_or_task_machinery() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("main.loom"),
        "concept Number { fn number(self Self) Int }\nimpl Number for Int { fn number(self Int) Int { self } }\nimpl Number for Bool { fn number(self Bool) Int { if self { 1 } else { 0 } } }\nfn sum[Ts... Number](values Ts...) Int { var total = 0\ncomptime for value in values { total = total + value.number() }\ntotal }\nfn pack[Ts...](values Ts...) (Ts...) { values }\nfn main() { let values = pack(40, 2, true)\nassert values.0 + values.1 == 42\nassert values.2\nassert sum(40, true, 1) == 42\nassert sum() == 0\nlet empty = pack()\ndiscard empty\ndiscard pack(pack()...) }").unwrap();
    let ir = directory.path().join("packs.ll");
    let executable = common::executable(directory.path(), "packs");
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
    let text = fs::read_to_string(ir).unwrap();
    assert!(!text.contains("call ptr @loom_"));
    assert!(!text.contains("@loom_task_"));
    success(&Command::new(executable).output().unwrap());
}
