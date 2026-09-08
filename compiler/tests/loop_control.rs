use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn loops_branch_directly_and_preserve_lexical_cleanup() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
fn nested() Int {
    var i = 0
    var total = 0
    while i < 6 {
        i = i + 1
        if i == 2 { continue }
        var j = 0
        while j < 4 {
            j = j + 1
            if j == 2 { continue }
            if j == 4 { break }
            total = total + i * 10 + j
        }
        if i == 4 { break }
    }
    total
}

fn cleanup() Int {
    var total = 0
    defer { total = 99999 }
    var i = 0
    while i < 4 {
        i = i + 1
        defer { total = total * 10 + 1 }
        {
            defer { total = total * 10 + 2 }
            if i == 1 { continue }
            break
        }
    }
    total
}

fn condition_control() Int {
    var outer = 0
    var total = 0
    while outer < 4 {
        outer = outer + 1
        while {
            if outer == 2 { continue }
            if outer == 4 { break }
            true
        } {
            total = total + 1
            break
        }
        total = total + 10
    }
    total
}

fn add(a Int, b Int) Int { a + b }

fn argument_control() Int {
    var i = 0
    var total = 0
    while i < 4 {
        i = i + 1
        total = total + add(5, {
            if i == 2 { continue }
            if i == 4 { break }
            i
        })
    }
    total
}

fn main() {
    assert nested() == 172
    assert cleanup() == 2121
    assert condition_control() == 22
    assert argument_control() == 14
    assert comptime { nested() + cleanup() + condition_control() + argument_control() } == 2329
}
"#,
    )
    .unwrap();
    success(&loom(&["check", package.path().to_str().unwrap()]));
    let executable = common::executable(package.path(), "loops");
    let ir = package.path().join("loops.ll");
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
        success(&Command::new(&executable).output().unwrap());
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(ir.contains("loom_rt_cleanup_push") && ir.contains("loom_rt_cleanup_pop"));
        assert!(!ir.contains("roots_enter") && !ir.contains("executor"));
        if level == "0" {
            assert!(ir.contains("br label %while.test"));
            assert!(ir.contains("br label %while.done"));
        }
    }
}

#[test]
fn loop_cleanup_keeps_managed_values_live_across_collection() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.list.new
import std.list.push
import std.list.get
import std.list.length
import std.text.concat
import std.int.to_text

fn collect(events List[Text]) {
    defer { push(events, "outer") }
    var i = 0
    while i < 4 {
        i = i + 1
        let saved = concat("saved ", to_text(i))
        defer { push(events, saved) }
        {
            defer { push(events, concat("inner ", to_text(i))) }
            if i == 1 { continue }
            break
        }
    }
    assert length(events) == 4
}

fn main() {
    let events = new[Text]()
    collect(events)
    assert length(events) == 5
    assert get(events, 0) == "inner 1"
    assert get(events, 1) == "saved 1"
    assert get(events, 2) == "inner 2"
    assert get(events, 3) == "saved 2"
    assert get(events, 4) == "outer"
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "managed-loops");
    success(&loom(&[
        "build",
        package.path().to_str().unwrap(),
        "--output",
        executable.to_str().unwrap(),
    ]));
    success(
        &Command::new(executable)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );
}
