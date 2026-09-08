use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn list_literals_allocate_known_capacity_without_push_dispatch() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.list.get
import std.list.set
import std.list.length

fn fresh() List[Int] { [1, 2, 3] }

fn control() Int {
    var trace = 0
    var i = 0
    while i < 3 {
        i = i + 1
        discard [
            {
                trace = trace + 1
                if i == 1 { continue }
                if i == 3 { break }
                i
            },
            { trace = trace + 10
                i },
        ]
    }
    trace
}

fn main() {
    var order = 0
    let values = [
        { order = order + 1
            order },
        { order = order + 1
            order },
    ]
    assert length(values) == 2 && get(values, 0) == 1 && get(values, 1) == 2
    let empty List[Int] = []
    assert length(empty) == 0
    let first = fresh()
    let second = fresh()
    let alias = first
    set(alias, 0, 9)
    assert get(first, 0) == 9 && get(second, 0) == 1
    assert control() == 13
    assert comptime { control() } == 13
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "literal");
    let ir = package.path().join("literal.ll");
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
        assert!(ir.contains("@loom_rt_list_new"));
        assert!(!ir.contains("reserve_one") && !ir.contains("buffer.grow"));
    }
}

#[test]
fn list_literal_snapshots_and_typed_elements_survive_moving_gc() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.list.get
import std.list.set
import std.list.push
import std.text.concat
import std.display.Display

record Payload { text Text }

impl Display for Payload {
    fn display(self Payload) Text { self.text }
}

fn increment(value Int) Int { value + 1 }
fn double(value Int) Int { value * 2 }

fn prepared() List[List[Text]] {
    comptime {
        let shared = [concat("prepared", "!")]
        [shared, shared]
    }
}

fn main() {
    let shared = [concat("old", "!")]
    let nested = [shared, shared]
    set(shared, 0, concat("shared", "!"))
    assert get(get(nested, 0), 0) == "shared!"
    push(shared, "grew")
    assert get(get(nested, 1), 1) == "grew"

    var saved = Payload { text = concat("saved", "!") }
    let records = [saved, {
        saved = Payload { text = concat("changed", "!") }
        Payload { text = concat("later", "!") }
    }]
    assert get(records, 0).text == "saved!" && get(records, 1).text == "later!"
    let displays List[dyn Display] = [7, concat("text", "!"), saved]
    assert get(displays, 0).display() == "7"
    assert get(displays, 1).display() == "text!"
    assert get(displays, 2).display() == "changed!"
    let callbacks List[fn(Int) Int] = [increment, double]
    assert get(callbacks, 0)(3) == 4 && get(callbacks, 1)(3) == 6

    let first = prepared()
    let second = prepared()
    set(get(first, 0), 0, concat("updated", "!"))
    assert get(get(first, 1), 0) == "updated!"
    assert get(get(second, 0), 0) == "prepared!"
    assert get(get(second, 1), 0) == "prepared!"
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "managed-literal");
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
