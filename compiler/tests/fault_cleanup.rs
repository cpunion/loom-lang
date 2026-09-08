use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn lexical_callbacks_survive_faults_and_moving_gc_without_unwinding() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.io.write_text
import std.text.concat
import std.text.slice
import std.int.to_text
import std.process.arguments
import std.list.get

enum Message { Some(Text) None }

fn say(value Text) { discard write_text(value) }

fn generated_failure() { assert false }

fn runtime_failure() {
    defer { say("inner|") }
    discard concat("move", " caller roots")
    discard slice("é", 0, 1)
}

fn late(runtime Bool) {
    var value = concat("old", "|")
    defer { say(value) }
    value = concat("late", "|")
    if runtime { runtime_failure() } else { generated_failure() }
}

fn nested(initial_fault Bool) {
    var value = concat("old", "|")
    defer { say(value) }
    defer {
        value = concat("changed", "|")
        if initial_fault { discard slice("é", 0, 1) } else { assert false }
    }
    if initial_fault { assert false }
}

fn saved() Text {
    var value = concat("saved", "|")
    defer { value = concat("changed", "|") }
    value
}

fn normal() {
    var assigned = 0
    {
        defer { assert assigned == 42 }
        defer { assigned = 42 }
    }
    var index = 0
    while index < 4 {
        index = index + 1
        defer {
            let own = Message.Some(concat(to_text(index), "|"))
            match own {
                Message.Some(text) => {
                    discard concat("move", " callback locals")
                    say(text)
                }
                Message.None => { assert false }
            }
        }
        if index == 1 { continue }
        break
    }
    say(saved())
}

fn main() {
    let mode = get(arguments(), 1)
    defer { say("outer|") }
    if mode == "normal" { normal() } else {
        if mode == "generated" { late(false) } else {
            if mode == "runtime" { late(true) } else {
                nested(mode == "nested")
            }
        }
    }
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "fault-cleanup");
    let ir = package.path().join("fault-cleanup.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package.path().to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        for (mode, expected, message) in [
            ("normal", "1|2|saved|outer|", None),
            ("generated", "late|outer|", Some("assertion failed")),
            ("runtime", "inner|late|outer|", Some("invalid text slice")),
            ("nested", "changed|outer|", Some("assertion failed")),
            ("normal-failure", "changed|outer|", Some("assertion failed")),
        ] {
            let output = Command::new(&executable)
                .arg(mode)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap();
            assert_eq!(
                output.stdout,
                expected.as_bytes(),
                "{level}/{mode}: {output:?}"
            );
            if let Some(message) = message {
                assert_eq!(output.status.code(), Some(1), "{level}/{mode}: {output:?}");
                let diagnostic = String::from_utf8_lossy(&output.stderr);
                assert!(diagnostic.contains(message), "{diagnostic}");
                assert_eq!(
                    diagnostic.matches("RuntimeFault:").count(),
                    1,
                    "{diagnostic}"
                );
            } else {
                success(&output);
                assert!(output.stderr.is_empty());
            }
        }
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(ir.contains("loom_rt_cleanup_push") && ir.contains("loom_rt_cleanup_pop"));
        assert!(ir.contains("loom_rt_fault"));
        assert!(!ir.contains("setjmp") && !ir.contains("landingpad") && !ir.contains("executor"));
    }
}
