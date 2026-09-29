use std::{fs, process::Command};

mod common;
use common::{loom, success};

#[test]
fn shared_views_preserve_identity_in_native_and_compile_time_execution() {
    let package = "compiler/examples/shared_views";
    success(&loom(&["check", package]));
    success(&loom(&["test", package]));
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "views");
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
fn views_do_not_duplicate_one_shot_elements() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    for text in [
        r#"
import std.list.view.capture

async fn value() Int {
    1
}

async fn main() {
    let view = capture([value()])
    discard view
}
"#,
        r#"
import std.list.view.capture
import std.resource.MustScope
import std.resource.Dispose
import std.resource.generate

record Guard {
    value Int
}

impl MustScope for Guard {
}

impl Dispose for Guard {
    fn dispose(self Guard) {
        discard self.value
    }
}

fn create(index Int) Guard {
    Guard { value = index }
}

fn main() {
    scoped resources = generate(2, create)
    discard capture(resources)
}
"#,
    ] {
        fs::write(&source, text).unwrap();
        let output = loom(&["check", temporary.path().to_str().unwrap()]);
        assert!(!output.status.success(), "one-shot view was accepted");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostic.contains("task")
                || diagnostic.contains("scoped")
                || diagnostic.contains("resource"),
            "{diagnostic}"
        );
    }
}
