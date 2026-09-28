use std::{fs, process::Command};

mod common;
use common::{loom, success};

#[test]
fn record_updates_preserve_source_semantics_without_new_runtime_operations() {
    let package = "compiler/examples/record_updates";
    for command in ["check", "test", "run"] {
        success(&loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "updates");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&executable));
    }

    let scalar = temporary.path().join("scalar");
    fs::create_dir(&scalar).unwrap();
    fs::write(
        scalar.join("main.loom"),
        r#"
record Pair {
    first Int
    second Bool
}

fn copy(value Pair, first Int) Pair
ensures result.first == first
ensures result.second == value.second
{
    Pair {
        first = first
        ..value
    }
}

fn main() {
    let before = Pair { first = 1, second = true }
    let after = copy(before, 2)
    assert before.first == 1 && after.first == 2 && after.second
}
"#,
    )
    .unwrap();
    let ir = temporary.path().join("scalar.ll");
    success(
        &common::command(&["build"])
            .arg(&scalar)
            .arg("--output")
            .arg(&executable)
            .arg("--emit-ir")
            .arg(&ir)
            .env("LOOM_OPT_LEVEL", "0")
            .output()
            .unwrap(),
    );
    assert!(!fs::read_to_string(ir).unwrap().contains("@loom_rt_"));
    success(&Command::new(executable).output().unwrap());
}
