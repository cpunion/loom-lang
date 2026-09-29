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
    let directory = common::root().join(package);
    let source = directory.join("main.loom");
    let text = fs::read_to_string(&source).unwrap();
    let output = loom(&[
        "editor-query",
        directory.to_str().unwrap(),
        "--at",
        source.to_str().unwrap(),
        &text.find("helper_call!(40)").unwrap().to_string(),
    ]);
    success(&output);
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("\"types\":[\"Int\"]"), "{report}");
    assert!(
        report
            .replace("\\\\", "/")
            .contains("generators/generate.loom"),
        "{report}"
    );
}

#[test]
fn generated_code_keeps_proof_and_one_shot_obligations() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    let cases = [
        (
            r#"
import std.reflect.Schema
import std.resource.MustScope
import std.resource.Dispose

record Guard {
    value Int
}

impl MustScope for Guard {}

impl Dispose for Guard {
    fn dispose(self Guard) {
        discard self.value
    }
}

fn ignore(types List[Schema]) Text {
    discard types
    "true"
}

fn main() {
    discard ignore!(Guard { value = 1 })
}
"#,
            "macro resource input must enter scoped",
        ),
        (
            r#"
import std.reflect.Schema
import std.resource.NoSuspend

record Guard {
    value Int
}

impl NoSuspend for Guard {}

fn wait(types List[Schema]) Text {
    discard types
    "$0.await"
}

async fn task() Int {
    1
}

async fn main() {
    let guard = Guard { value = 0 }
    discard wait!(task())
    discard guard
}
"#,
            "NoSuspend value is still in scope",
        ),
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

#[test]
fn structured_macro_output_requires_valid_source_syntax() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("main.loom");
    for (kind, value, message) in [
        ("Name", "f()", "syntax tree"),
        ("Binary", "+", "missing a child"),
        ("ExpansionId", "fake", "internal macro syntax"),
    ] {
        fs::write(
            &path,
            format!(
                r#"
import std.reflect.Schema
import std.loom.ast.Node
import std.loom.ast.NodeKind
import std.loom.source.Span
import std.list.new

fn generate(types List[Schema]) Node {{
    discard types
    Node {{
        kind = NodeKind.{kind}
        value = "{value}"
        span = Span {{
            start = 0
            end = 0
        }}
        children = new[Node]()
    }}
}}

fn main() {{
    discard generate!()
}}
"#
            ),
        )
        .unwrap();
        let result = common::command(&["check"])
            .arg(temporary.path())
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(message),
            "{result:?}"
        );
    }
}

#[test]
fn source_ast_tool_generates_a_normally_checked_package() {
    let temporary = tempfile::tempdir().unwrap();
    let generator = common::executable(temporary.path(), "generator");
    success(
        &common::command(&["build", "compiler/examples/ast_generation"])
            .arg("--output")
            .arg(&generator)
            .output()
            .unwrap(),
    );
    let generated = Command::new(generator).output().unwrap();
    success(&generated);
    let package = temporary.path().join("generated");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("main.loom"), generated.stdout).unwrap();
    for operation in ["check", "test", "run"] {
        success(
            &common::command(&[operation])
                .arg(&package)
                .output()
                .unwrap(),
        );
    }
}
