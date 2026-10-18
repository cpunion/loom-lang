use std::{fs, process::Command};

mod common;
use common::{loom, success};

#[test]
fn typed_json_is_a_source_library_with_lexical_visibility() {
    let package = "compiler/examples/json_encoding";
    for command in ["check", "test", "run"] {
        success(&loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "json-encoding");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
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
fn reflection_is_typed_source_data_without_runtime_discovery() {
    success(&loom(&["test", "compiler/examples/reflection"]));
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "reflection");
    let ir = temporary.path().join("reflection.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", "compiler/examples/reflection"])
                .arg("--output")
                .arg(&executable)
                .arg("--emit-ir")
                .arg(&ir)
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
        let lowered = fs::read_to_string(&ir).unwrap();
        assert!(!lowered.contains("reflection_method_should_stay_dead"));
        assert!(!lowered.contains("predicate_helper_should_stay_dead"));
        assert!(!lowered.contains("loom_rt_reflect"));
    }

    let package = temporary.path().join("scalar");
    fs::create_dir(&package).unwrap();
    fs::write(
        package.join("main.loom"),
        r#"
import std.reflect.describe
import std.reflect.Kind
import std.reflect.from_fields

record Pair {
    count Int
    enabled Bool
}

fn integer[T]() Int {
    comptime if describe[T]().types[0].kind == Kind.Int {
        1
    } else {
        0
    }
}

fn main() {
    assert integer[Int]() == 1 && integer[Bool]() == 0
    let fields = comptime map Field in Pair {
        comptime if Field == Int {
            42
        } else {
            true
        }
    }
    let pair Pair = from_fields(fields)
    assert pair.count == 42 && pair.enabled
}
"#,
    )
    .unwrap();
    success(
        &common::command(&["build"])
            .arg(&package)
            .arg("--output")
            .arg(&executable)
            .arg("--emit-ir")
            .arg(&ir)
            .env("LOOM_OPT_LEVEL", "0")
            .output()
            .unwrap(),
    );
    let lowered = fs::read_to_string(&ir).unwrap();
    assert!(!lowered.contains("@loom_rt_"));
    success(&Command::new(&executable).output().unwrap());
}

#[test]
fn weakening_witnesses_cannot_be_forged_or_rebound() {
    let temporary = tempfile::tempdir().unwrap();
    let package = temporary.path();
    let prefix = r#"
import std.reflect.predicates.Weakening
import std.reflect.predicates.weakening
import std.reflect.from_fields
import std.option.Option

type Positive = Int where self > 0
type Nonnegative = Int where self >= 0

"#;
    for (body, expected) in [
        (
            "fn main() { let forged = Weakening[Nonnegative, Positive] {} }",
            "initialize every record field",
        ),
        (
            r#"
fn main() {
    match weakening[Positive, Nonnegative]() {
        Option.Some(genuine) => {
            let forged = Weakening[Nonnegative, Positive] { state = genuine.state }
        }
        Option.None => {}
    }
}
"#,
            "expected std.reflect.predicates.Checked[Nonnegative, Positive], found std.reflect.predicates.Checked[Positive, Nonnegative]",
        ),
        (
            r#"
fn main() {
    match weakening[Positive, Nonnegative]() {
        Option.Some(genuine) => {
            let forged Weakening[Nonnegative, Positive] = from_fields((genuine.state,))
        }
        Option.None => {}
    }
}
"#,
            "expected std.reflect.predicates.Checked[Nonnegative, Positive], found std.reflect.predicates.Checked[Positive, Nonnegative]",
        ),
    ] {
        fs::write(package.join("main.loom"), format!("{prefix}{body}\n")).unwrap();
        let output = common::command(&["check"]).arg(package).output().unwrap();
        assert!(!output.status.success(), "forged witness accepted: {body}");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostic.contains(expected),
            "unexpected rejection: {diagnostic}"
        );
    }
}
