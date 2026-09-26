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
