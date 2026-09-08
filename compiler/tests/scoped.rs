use std::{fs, process::Command};
mod common;
use common::{loom, success};

const RESOURCES: &str = r#"
import std.resource.Dispose
import std.resource.MustScope
import std.result.Result
import std.list.push
import std.list.length

record Guard {
    id Int
    trace List[Int]
}
impl Dispose for Guard {
    fn dispose(self Guard) { push(self.trace, self.id) }
}
concept Touch { fn touch(self Self) }
impl Touch for Guard {
    fn touch(self Guard) { push(self.trace, self.id * 10) }
}

record Required {
    id Int
    trace List[Int]
}
impl MustScope for Required {}
impl Dispose for Required {
    fn dispose(self Required) { push(self.trace, self.id) }
}
impl Touch for Required {
    fn touch(self Required) { push(self.trace, self.id * 10) }
}

fn guard(id Int, trace List[Int]) Guard { Guard { id = id, trace = trace } }
fn required(id Int, trace List[Int]) Required { Required { id = id, trace = trace } }
fn attempt(id Int, trace List[Int], ok Bool) Result[Required, Text] {
    if ok { Result.Ok(required(id, trace)) } else { Result.Err("unavailable") }
}
"#;

#[test]
fn scoped_factories_payloads_and_methods_keep_block_lifetimes() {
    let package = tempfile::tempdir().unwrap();
    fs::write(package.path().join("main.loom"), format!("{RESOURCES}{}", r#"
fn acquired(trace List[Int]) Result[Int, Text] {
    scoped first = required(7, trace)
    scoped second Required = attempt(8, trace, true)?
    Result.Ok(42)
}
fn unavailable(trace List[Int]) Result[Int, Text] {
    scoped first = required(9, trace)
    scoped absent = attempt(10, trace, false)?
    Result.Ok(0)
}

fn main() {
    let trace List[Int] = []
    {
        scoped outer = guard(1, trace)
        outer.touch()
        defer { push(trace, 2) }
        if true {
            scoped inner Guard = guard(3, trace)
            inner.touch()
        }
        defer { push(trace, 4) }
    }
    assert length(trace) == 6
    assert trace[0] == 10 && trace[1] == 30 && trace[2] == 3
    assert trace[3] == 4 && trace[4] == 2 && trace[5] == 1

    // Dispose alone does not make ordinary bindings linear or auto-disposed.
    {
        let plain = guard(6, trace)
        let alias = plain
        alias.touch()
        assert plain.id == 6
    }
    assert length(trace) == 7 && trace[6] == 60
    assert match acquired(trace) { Result.Ok(value) => value == 42, Result.Err(_) => false }
    assert length(trace) == 9 && trace[7] == 8 && trace[8] == 7
    assert match unavailable(trace) { Result.Err(message) => message == "unavailable", Result.Ok(_) => false }
    assert length(trace) == 10 && trace[9] == 9
    match attempt(11, trace, true) {
        Result.Ok(payload) => {
            scoped resource = payload
            resource.touch()
        }
        Result.Err(_) => { assert false }
    }
    assert length(trace) == 12 && trace[10] == 110 && trace[11] == 11
}
"#)).unwrap();
    let executable = common::executable(package.path(), "scoped");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package.path().to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
            ])
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
fn scoped_faults_clean_only_registered_values_and_reject_escape() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        format!(
            "{RESOURCES}{}",
            r#"
import std.io.write_text
import std.text.concat
import std.text.slice
import std.int.to_text
import std.process.arguments

fn failing(trace List[Int]) Guard {
    assert false
    guard(2, trace)
}
fn report(trace List[Int]) {
    var index = 0
    while index < length(trace) {
        discard write_text(concat(to_text(trace[index]), "|"))
        index = index + 1
    }
}
fn main() {
    let mode = arguments()[1]
    let trace List[Int] = []
    defer { report(trace) }
    scoped first = guard(1, trace)
    if mode == "initializer" {
        scoped absent = failing(trace)
    } else {
        scoped second = guard(2, trace)
        defer { discard slice("é", 0, 1) }
        assert false
    }
}
"#
        ),
    )
    .unwrap();
    let executable = common::executable(package.path(), "scoped-fault");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package.path().to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        for (mode, expected) in [("initializer", "1|"), ("body", "2|1|")] {
            let output = Command::new(&executable)
                .arg(mode)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1), "{level}/{mode}: {output:?}");
            assert_eq!(
                output.stdout,
                expected.as_bytes(),
                "{level}/{mode}: {output:?}"
            );
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            assert!(diagnostic.contains("assertion failed"), "{diagnostic}");
            assert_eq!(
                diagnostic.matches("RuntimeFault:").count(),
                1,
                "{diagnostic}"
            );
            assert!(!diagnostic.contains("invalid text slice"), "{diagnostic}");
        }
    }
    for rejected in [
        "fn main() { scoped value = guard(1, [])\nlet copied = value\ndiscard copied }",
        "fn main() { scoped value = guard(1, [])\nvalue = guard(2, []) }",
        "fn escape(trace List[Int]) Guard { scoped value = guard(1, trace)\nvalue }\nfn main() { discard escape([]) }",
        "fn keep(value Guard) {}\nfn main() { scoped value = guard(1, [])\nkeep(value) }",
        "fn main() { scoped value = guard(1, [])\nvalue.dispose() }",
        "fn main() { discard required(1, []) }",
        "fn main() { match attempt(1, [], true) { Result.Ok(value) => { discard value }, Result.Err(_) => {} } }",
    ] {
        fs::write(
            package.path().join("main.loom"),
            format!("{RESOURCES}\n{rejected}"),
        )
        .unwrap();
        let output = loom(&["check", package.path().to_str().unwrap()]);
        assert_eq!(
            output.status.code(),
            Some(1),
            "accepted {rejected}: {output:?}"
        );
        assert!(!output.stderr.is_empty());
    }
}
