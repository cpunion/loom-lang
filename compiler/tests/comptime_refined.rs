use std::{fs, process::Command};
mod common;
use common::success;

const SOURCE: &str = r#"
import std.list.length
import std.list.push

type Nonempty[T] = List[T] where length(self) > 0

fn positive(values List[Int]) Bool {
    var index = 0
    while index < length(values) {
        if values[index] <= 0 {
            return false
        }
        index = index + 1
    }
    length(values) > 0
}

type Positives = List[Int] where positive(self)

fn pair() (Nonempty[Int], Nonempty[Int]) {
    comptime {
        let values = Nonempty([1, 2])
        (values, values)
    }
}

fn captured() fn() Nonempty[Int] {
    comptime {
        let values = Nonempty([4, 5])
        fn() Nonempty[Int] {
            values
        }
    }
}

fn main() {
    let positive_values = comptime { Positives([1, 2]) }
    assert positive_values[0] == 1
    let values = pair()
    push(values.0, 3)
    assert length(values.1) == 3
    assert length(pair().0) == 2
    let nested = comptime {
        let inner = [1]
        (Nonempty([inner]), inner)
    }
    push(nested.0[0], 2)
    assert length(nested.0) == 1 && length(nested.1) == 2
    let read = captured()
    push(read(), 6)
    assert length(read()) == 3
    assert length(captured()()) == 2
}

test fn restored_graph() {
    main()
}
"#;

#[test]
fn refined_computations_keep_admission_aliases_and_cache_validation() {
    let package = tempfile::tempdir().unwrap();
    let path = package.path().to_str().unwrap();
    let source = package.path().join("main.loom");
    let cache = package.path().join("cache");
    let check = || common::loom(&["check", path, "--frontend-cache", cache.to_str().unwrap()]);
    fs::write(&source, SOURCE).unwrap();
    success(&check());
    success(&check());
    fs::write(
        &source,
        SOURCE.replace("length(self) > 0", "length(self) > 2"),
    )
    .unwrap();
    let changed = check();
    assert!(!changed.status.success());
    assert!(!changed.stderr.is_empty());
    fs::write(&source, SOURCE).unwrap();
    success(&check());

    let artifact = common::executable(package.path(), "restored");
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

    for rejected in [
        r#"
import std.list.length
import std.list.set
fn positive(values List[Int]) Bool {
    var index = 0
    while index < length(values) {
        if values[index] <= 0 {
            return false
        }
        index = index + 1
    }
    length(values) > 0
}
type Positives = List[Int] where positive(self)
pub fn changed_content() {
    let values = comptime { Positives([1, 2]) }
    set(values, 0, -1)
}
"#,
        r#"
import std.list.length
type Nonempty = List[Int] where length(self) > 0
pub fn exposed() List[Int] {
    comptime { Nonempty([1, 2]) }
}
"#,
        r#"
import std.list.length
type Nonempty = List[Int] where length(self) > 0
pub fn exposed_capture() fn() List[Int] {
    comptime {
        let values = Nonempty([1, 2])
        fn() List[Int] {
            values
        }
    }
}
"#,
        r#"
import std.list.length
import std.list.push
type Fixed = List[Int] where length(self) == 2
pub fn resized() {
    let values = comptime { Fixed([1, 2]) }
    push(values, 3)
}
"#,
    ] {
        fs::write(&source, rejected).unwrap();
        let invalid = check();
        assert!(!invalid.status.success());
        assert!(!invalid.stderr.is_empty());
    }
}
