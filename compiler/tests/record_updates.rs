use std::{fs, process::Command};

mod common;
use common::{loom, success};

#[test]
fn local_field_reads_load_only_the_selected_inline_value() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(
        temporary.path().join("main.loom"),
        r#"
import std.text.concat
import std.list.new
import std.list.push
import std.list.length

record Payload {
    label Text
    number Int
}

record Envelope {
    tag Int
    payload Payload
}

fn number(value Envelope) Int {
    value.payload.number
}

fn make(log List[Int], label Text, amount Int) Envelope {
    push(log, amount)
    Envelope {
        tag = 7
        payload = Payload {
            label = concat(label, "!")
            number = amount
        }
    }
}

fn join(first Text, second Text) Text {
    concat(first, second)
}

fn exercise() {
    let log = new[Int]()
    var saved = make(log, "first", 42)
    discard concat("collect", "now")
    assert number(saved) == 42 && saved.tag == 7
    assert make(log, "computed", 9).payload.number == 9
    assert length(log) == 2
    let snapshot = join(saved.payload.label, {
        saved = make(log, "second", 64)
        discard concat("collect", "again")
        concat("end", "")
    })
    assert snapshot == "first!end"
    assert saved.payload.label == "second!" && number(saved) == 64
    assert length(log) == 3
}

fn main() {
    exercise()
}

test fn fields() {
    exercise()
}
"#,
    )
    .unwrap();
    success(
        &common::command(&["check"])
            .arg(temporary.path())
            .output()
            .unwrap(),
    );
    let executable = common::executable(temporary.path(), "fields");
    let ir = temporary.path().join("fields.ll");
    for level in ["0", "2"] {
        for mode in ["build", "test"] {
            let mut command = common::command(&[mode]);
            command.arg(temporary.path());
            if mode == "test" {
                command.arg("--no-run");
            }
            success(
                &command
                    .arg("--output")
                    .arg(&executable)
                    .arg("--emit-ir")
                    .arg(&ir)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            success(&common::run_tasks(&executable));
        }
        if level == "0" {
            let llvm = fs::read_to_string(&ir).unwrap();
            let getter = llvm
                .split("\ndefine ")
                .find(|body| {
                    body.starts_with("internal i64 @loom.fn.")
                        && body
                            .lines()
                            .next()
                            .unwrap()
                            .contains("{ i64, { ptr, i64 } }")
                })
                .unwrap()
                .split("\n}")
                .next()
                .unwrap();
            assert!(getter.contains("field.address") && getter.contains("load i64"));
            assert!(!getter.contains("extractvalue") && !getter.contains("load {"));
        }
    }
}

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
