use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn associated_defaults_and_bounds_specialize_static_and_dynamic_calls() {
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("main.loom"),
        r#"
import std.display.Display
import std.text.concat
concept Source {
    type Item Display = Text
    fn first(self Self) Self.Item
    fn label(self Self) Text { self.first().display() }
    fn unused(self Self) Int { 9001 }
}
record Named { text Text }
impl Source for Named { fn first(self Named) Text { self.text } }
record Box[T] { item T }
impl[T Display] Source for Box[T] {
    type Item = T
    fn first(self Box[T]) T { self.item }
}
fn read[S Source](source S) Text { source.first().display() }
fn erase[S Source](source S) dyn Source[Item = S.Item] { source }
fn main() {
    assert read(Named { text = concat("de", "fault") }) == "default"
    assert read(Box { item = 42 }) == "42"
    let erased = erase(Box { item = concat("dy", "namic") })
    assert erased.label() == "dynamic"
    let constant = comptime { read(Box { item = true }) }
    assert constant == "true"
}
"#,
    )
    .unwrap();
    let executable = common::executable(source.path(), "associated");
    let ir = source.path().join("associated.ll");
    success(
        &common::command(&[
            "build",
            source.path().to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(
        &Command::new(executable)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );
    assert!(!fs::read_to_string(ir).unwrap().contains("9001"));
}

#[test]
fn generic_conformances_specialize_nested_receivers_without_runtime_evidence() {
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("main.loom"),
        r#"
concept Value { fn value(self Self) Int }
record Wrap[T] { inner T }
impl Value for Int { fn value(self Int) Int { self } }
impl[T Value] Value for Wrap[T] { fn value(self Wrap[T]) Int { self.inner.value() } }
fn forward[T Value](value T) Int { value.value() }
fn main() { assert forward(Wrap { inner = Wrap { inner = 42 } }) == 42 }
"#,
    )
    .unwrap();
    let executable = common::executable(source.path(), "generic");
    let ir = source.path().join("generic.ll");
    success(
        &common::command(&[
            "build",
            source.path().to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(&Command::new(executable).output().unwrap());
    let ir = fs::read_to_string(ir).unwrap();
    assert_eq!(
        ir.lines()
            .filter(|line| line.starts_with("define ") && line.contains("@loom.fn."))
            .count(),
        5
    );
    assert!(!ir.contains("loom_rt_") && !ir.contains("@loom.witness"));
}

#[test]
fn static_concepts_emit_only_selected_calls_and_keep_native_value_layouts() {
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("main.loom"),
        "concept Value { fn value(self Self) Int\nfn doubled(self Self) Int { self.value() + self.value() }\nfn unused(self Self) Int { 9001 } }\nrecord Used { n Int }\nrecord Unused { n Int }\nimpl Value for Used { fn value(self Used) Int { self.n } }\nimpl Value for Unused { fn value(self Unused) Int { self.n + 9000 } }\nfn forward[T Value](value T) Int { value.doubled() }\nfn main() { assert forward(Used { n = 42 }) == 84 }",
    )
    .unwrap();
    let executable = common::executable(source.path(), "static");
    let ir = source.path().join("static.ll");
    success(
        &common::command(&[
            "build",
            source.path().to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(&Command::new(executable).output().unwrap());
    let ir = fs::read_to_string(ir).unwrap();
    // The uncalled implementation never reaches LLVM, even with optimization off.
    assert_eq!(
        ir.lines()
            .filter(|line| line.starts_with("define ") && line.contains("@loom.fn."))
            .count(),
        4
    );
    assert!(!ir.contains("loom_rt_"));
    assert!(!ir.contains("9000") && !ir.contains("9001"));

    let example = common::root().join("compiler/examples/concepts");
    let executable = common::executable(source.path(), "managed");
    success(
        &common::command(&[
            "build",
            example.to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(
        &Command::new(executable)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );
}
