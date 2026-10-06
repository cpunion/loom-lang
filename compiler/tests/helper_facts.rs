use std::{fs, process::Command};
mod common;
use common::success;

const PROGRAM: &str = r#"
import std.float.abs
import std.float.is_nan
import std.list.length
import std.list.push

type Positive = Int where self > 0
type NonNegative = Float where self >= 0.0
type NonEmpty = Text where self != ""

record Snapshot {
    count Int
    values List[Int]
}

type Ready = Snapshot where self.count > 0

fn positive(value Int) Bool {
    value > 0
}

fn nonempty(value Text) Bool {
    value != ""
}

fn selected[T](value T, comptime predicate fn(T) Bool) Bool {
    predicate(value)
}

fn keep(value Text) NonEmpty
requires selected(value, nonempty) {
    let copy = value
    NonEmpty(copy)
}

fn checked(value Int) Positive
requires positive(value) {
    Positive(value)
}

fn copied(value Int) Positive {
    let condition = positive(value)
    assert condition
    let copy = value
    Positive(copy)
}

fn magnitude(value Float) NonNegative
requires !is_nan(value) {
    NonNegative(abs(value))
}

fn main() {
    assert checked(7) == 7
    assert copied(9) == 9
    assert magnitude(-3.5) == 3.5
    assert magnitude(-1.0 / 0.0) == 1.0 / 0.0
    assert 1.0 / magnitude(-0.0) == 1.0 / 0.0
    assert comptime { checked(5) } == 5
    assert keep("Loom") == "Loom"
    assert comptime { keep("source") } == "source"
    let pair = comptime {
        let shared = [1, 2]
        let ready = Ready(Snapshot {
            count = 1
            values = shared
        })
        (ready, shared)
    }
    push(pair.0.values, 3)
    assert length(pair.1) == 3
}

test fn guards() {
    main()
}
"#;

#[test]
fn pure_guard_constructions_compile_and_revalidate_helper_edits() {
    let package = tempfile::tempdir().unwrap();
    let path = package.path().to_str().unwrap();
    let source = package.path().join("main.loom");
    let cache = package.path().join("cache");
    let check = || common::loom(&["check", path, "--frontend-cache", cache.to_str().unwrap()]);
    fs::write(&source, PROGRAM).unwrap();
    success(&check());
    success(&check());
    fs::write(
        &source,
        PROGRAM.replace("    value > 0\n", "    value >= -1\n"),
    )
    .unwrap();
    let changed = check();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("expected Positive"));
    fs::write(&source, PROGRAM).unwrap();
    success(&check());
    let artifact = common::executable(package.path(), "helper-facts");
    for level in ["0", "2"] {
        success(
            &common::command(&["test", path])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &common::command(&["build", path, "--output", artifact.to_str().unwrap()])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &Command::new(&artifact)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
    success(&common::loom(&["run", path]));
}
