use std::fs;

mod common;
use common::success;

#[test]
fn dependent_store_arguments_revalidate_when_workers_enter_a_cached_build() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    let cache = package.path().join("cache");
    let declarations = r#"
import std.list.length

type Pair = List[Int] where length(self) == 2 && self[0] <= self[1]

fn copy_second(values Pair) {
    values[{
        let first = 0
        first
    }] = {
        let second = values[1]
        if second >= values[0] {
            second
        } else {
            values[0]
        }
    }
}

fn copy_first(values Pair) {
    values[1] = values[0]
}

type Flags = List[Bool] where length(self) > 0 && self[0]
type Words = List[Text] where length(self) > 0 && self[0] == "ready"
type Samples = List[Float] where length(self) > 0 && self[0] >= 0.0

fn set_flag(values Flags) {
    values[0] = true
}

fn set_word(values Words) {
    values[0] = "ready"
}

fn set_sample(values Samples) {
    values[0] = 0.0
}
"#;
    for shared in [false, true, false] {
        let entry = if shared {
            r#"
import std.task.worker.run

async fn main() {
    let values = Pair([1, 2])
    let first = run(fn() {
            copy_second(values)
        })
    let second = run(fn() {
            copy_first(values)
        })
    discard (first, second).await
}
"#
        } else {
            r#"
fn main() {
    let values = Pair([1, 2])
    copy_second(values)
    copy_first(values)
    assert values[0] == 2 && values[1] == 2
    let flags = Flags([true])
    let words = Words(["ready"])
    let samples = Samples([1.0])
    set_flag(flags)
    set_word(words)
    set_sample(samples)
    assert flags[0] && words[0] == "ready" && samples[0] == 0.0
}
"#
        };
        fs::write(&source, format!("{declarations}\n{entry}")).unwrap();
        let output = common::command(&["check", package.path().to_str().unwrap()])
            .arg("--frontend-cache")
            .arg(&cache)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), !shared, "{output:?}");
        if shared {
            let message = String::from_utf8_lossy(&output.stderr);
            assert!(message.contains("constrained sequence"), "{message}");
        } else {
            success(&common::loom(&["run", package.path().to_str().unwrap()]));
        }
    }
}

#[test]
fn scoped_arguments_keep_the_receiver_captured_before_rebinding() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    let contents = r#"
import std.list.length

type Head = List[Int] where length(self) == 2 && self[0] > 0

fn rebind(first Head, second Head) {
    var current = first
    current[0] = {
        current = second
        7
    }
    assert current[0] == second[0]
}

fn main() {
    let first = Head([1, 2])
    let second = Head([2, 3])
    rebind(first, second)
    assert first[0] == 7 && second[0] == 2
}

test fn scoped_receiver_is_evaluated_once() {
    main()
}
"#;
    fs::write(&source, contents).unwrap();
    for mode in ["check", "test", "run"] {
        success(&common::loom(&[mode, package.path().to_str().unwrap()]));
    }
    let binary = common::executable(package.path(), "scoped-store");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package.path().to_str().unwrap()])
                .arg("--output")
                .arg(&binary)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&binary));
    }
    fs::write(&source, contents.replace("        7\n", "        0\n")).unwrap();
    let output = common::loom(&["check", package.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("constrained sequence"));
}

#[test]
fn cached_callback_return_guarantees_do_not_hide_intermediate_writes() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    let cache = package.path().join("cache");
    let valid = r#"
import std.list.length

type Head = List[Int] where length(self) > 0 && self[0] > 0

fn temporary(values List[Int]) Int
ensures result == 7
{
    values[0] = 7
    7
}

fn indirect(values List[Int]) Int {
    let action = temporary
    action(values)
}

pub fn update(values Head) {
    values[0] = indirect(values)
}

fn main() {
    let values = Head([1])
    update(values)
    assert values[0] == 7
}
"#;
    let invalid = valid.replace(
        "    values[0] = 7\n",
        "    values[0] = 0\n    values[0] = 7\n",
    );
    for (candidate, accepted) in [(valid, true), (invalid.as_str(), false), (valid, true)] {
        fs::write(&source, candidate).unwrap();
        let checked = common::command(&["check", package.path().to_str().unwrap()])
            .arg("--frontend-cache")
            .arg(&cache)
            .output()
            .unwrap();
        assert_eq!(checked.status.success(), accepted, "{checked:?}");
        if accepted {
            success(&common::loom(&["run", package.path().to_str().unwrap()]));
        } else {
            assert!(String::from_utf8_lossy(&checked.stderr).contains("constrained sequence"));
        }
    }
}
