use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn first_class_types_select_native_types_without_runtime_type_tags() {
    let example = common::root().join("compiler/examples/type_values");
    success(&loom(&["check", example.to_str().unwrap()]));
    for level in ["0", "2"] {
        success(
            &common::command(&["test", example.to_str().unwrap()])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &Command::new(common::executable(&example.join("target"), "tests"))
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
    success(&loom(&["run", example.to_str().unwrap()]));

    let folder = tempfile::tempdir().unwrap();
    fs::write(
        folder.path().join("main.loom"),
        r#"
import std.list.length
import std.list.new
import std.list.push
import std.meta.of
import std.meta.parameter_types
import std.meta.return_type
import std.meta.tuple
import std.meta.function
import std.option.Option

fn choose(flag Bool) type {
    if flag { Int } else { Text }
}
fn unused_callback(value Int, flag Bool) Text {
    assert false
    "never called"
}
fn unused_no_result() {
    assert false
}
pub fn answer() Int {
    assert comptime {
        let types = new[type]()
        push(types, Int)
        push(types, of[type]())
        assert types[0] == Int && types[1] == type
        let nested = new[List[type]]()
        push(nested, types)
        assert nested[0][1] == type
        assert of[fn(Int, Bool) Text]() == of[fn(Int, Bool) Text]()
        let parameters = parameter_types(unused_callback)
        assert length(parameters) == 2 && parameters[0] == Int && parameters[1] == Bool
        let declared = parameter_types[fn(Int, Bool) Text]()
        assert length(declared) == 2 && declared[0] == Int && declared[1] == Bool
        assert tuple(declared) == of[(Int, Bool)]()
        assert function(declared, return_type(unused_callback)) == of[fn(Int, Bool) Text]()
        assert function(parameter_types(unused_no_result), return_type(unused_no_result)) == of[fn()]()
        assert length(parameter_types[fn()]()) == 0
        assert match return_type[fn()]() {
            Option.None => true
            Option.Some(_) => false
        }
        assert match return_type[fn(Int) Task[Bool]]() {
            Option.Some(output) => output == of[Task[Bool]]()
            Option.None => false
        }
        assert length(parameter_types(unused_no_result)) == 0
        assert match return_type(unused_no_result) {
            Option.None => true
            Option.Some(_) => false
        }
        match return_type(unused_callback) {
            Option.Some(output) => output == Text
            Option.None => false
        }
    }
    let Selected = comptime { choose(true) }
    let value Selected = 42
    let Pair = comptime { tuple([Int, Text]) }
    let pair Pair = (value, "answer")
    pair.0
}
fn main() {
    assert answer() == 42
}
"#,
    )
    .unwrap();
    let output = common::executable(folder.path(), "app");
    let ir = folder.path().join("app.ll");
    success(
        &common::command(&[
            "build",
            folder.path().to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(&Command::new(output).output().unwrap());
    let text = fs::read_to_string(ir).unwrap();
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("define ") && line.contains("@loom.fn."))
            .count(),
        2
    );
    assert!(text.contains("i64 42"));
}

#[test]
fn signature_metadata_requires_conformance_tuple_shape_and_compile_time_use() {
    let folder = tempfile::tempdir().unwrap();
    for body in [
        r#"
fn main() {
    discard comptime { parameter_types[Int]() }
}
"#,
        r#"
record Malformed {}
impl Signature for Malformed {
    type Parameters = List[Int]
    type Output = Int
}
fn main() {
    discard comptime { parameter_types[Malformed]() }
}
"#,
        r#"
fn main() {
    let inputs = parameter_types[fn(Int) Bool]()
    discard inputs
}
"#,
    ] {
        fs::write(
            folder.path().join("main.loom"),
            format!("import std.meta.parameter_types\nimport std.meta.Signature\n{body}"),
        )
        .unwrap();
        let output = loom(&["check", folder.path().to_str().unwrap()]);
        assert!(
            !output.status.success(),
            "accepted invalid metadata use: {body}"
        );
    }
}
