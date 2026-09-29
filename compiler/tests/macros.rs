use std::{fs, process::Command};

mod common;
use common::{loom, success};

#[test]
fn typed_macros_execute_without_a_runtime_generator() {
    let package = "compiler/examples/typed_macros";
    success(&loom(&["check", package]));
    success(&loom(&["test", package]));
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "macros");
    let ir = temporary.path().join("macros.ll");
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
        success(
            &Command::new(&executable)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        let lowered = fs::read_to_string(&ir).unwrap();
        assert!(!lowered.contains("$0"));
        assert!(!lowered.contains("loom_rt_macro"));
    }
}

#[test]
fn generated_code_keeps_proof_and_one_shot_obligations() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    let cases = [
        (
            r#"
import std.reflect.Schema

fn generate(types List[Schema]) Text {
    discard types
    "0"
}

fn positive() Int
ensures result > 0
{
    generate!()
}
"#,
            "not proved",
        ),
        (
            r#"
import std.reflect.Schema

fn generate(types List[Schema]) Text {
    discard types
    "($0, $0)"
}

async fn task() Int {
    1
}

async fn main() {
    let (first, second) = generate!(task())
    discard first.await
    discard second.await
}
"#,
            "task",
        ),
        (
            r#"
import std.reflect.Schema
import std.io.write_text

fn generate(types List[Schema]) Text {
    discard types
    discard write_text("must not execute during compilation")
    "1"
}

fn main() {
    discard generate!()
}
"#,
            "compile-time",
        ),
    ];
    for (text, diagnostic) in cases {
        fs::write(&source, text).unwrap();
        let output = common::command(&["check"])
            .arg(temporary.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(message.contains(diagnostic), "{message}");
    }
}
