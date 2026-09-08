use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn subscripts_preserve_operand_order_shared_storage_and_managed_snapshots() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.list.push
import std.list.length
import std.bytes.new
import std.bytes.push
import std.text.concat

fn receiver(values List[Int], trace List[Int]) List[Int] {
    trace[0] = trace[0] * 10 + 1
    values
}
fn position(values List[Int], trace List[Int]) Int {
    trace[0] = trace[0] * 10 + 2
    std.list.push(values, 20)
    1
}
fn replacement(values List[Int], trace List[Int]) Int {
    trace[0] = trace[0] * 10 + 3
    std.list.push(values, 30)
    99
}
fn identity[T](value T) T { value }
fn increment(value Int) Int { value + 1 }
fn double(value Int) Int { value * 2 }

record Holder { values List[Text] }

fn skipped() Int {
    let values = [0]
    var step = 0
    while step < 3 {
        step = step + 1
        values[{
            if step == 1 { continue }
            if step == 3 { break }
            0
        }] = step
    }
    values[0]
}

fn main() {
    let values = [10]
    let alias = values
    let trace = [0]
    receiver(values, trace)[position(values, trace)] = replacement(values, trace)
    assert trace[0] == 123 && length(alias) == 3
    assert alias[0] == 10 && alias[1] == 99 && alias[2] == 30

    var selected = [concat("old", "!")]
    let original = selected
    selected[{
        selected = [concat("new", "!")]
        0
    }] = concat("changed", "!")
    assert original[0] == "changed!" && selected[0] == "new!"
    let holder = Holder { values = original }
    holder.values[0] = concat("field", "!")
    let matrix = [holder.values, selected]
    matrix[0][0] = concat("nested", "!")
    assert original[0] == "nested!" && matrix[1][0] == "new!"

    let functions List[fn(Int) Int] = [increment, double]
    let index = 1
    assert functions[index](identity[Int](3)) == 6
    assert functions[0](3) == 4
    let bytes = std.bytes.new()
    std.bytes.push(bytes, 0)
    let shared = bytes
    shared[0] = 255
    assert bytes[0] == 255
    assert skipped() == 2 && comptime { skipped() } == 2
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "indexing");
    let ir = package.path().join("indexing.ll");
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
        success(
            &Command::new(&executable)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(!ir.contains("loom_rt_list_get") && !ir.contains("loom_rt_list_set"));
        assert!(!ir.contains("loom_rt_bytes_get") && !ir.contains("loom_rt_bytes_set"));
    }
}

#[test]
fn subscript_faults_preserve_assignment_evaluation_order_and_cleanup() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"
import std.io.write_text
import std.process.arguments
import std.bytes.new
import std.bytes.push

fn rhs() Int { discard write_text("rhs|")
    9 }

fn main() {
    let mode = arguments()[1]
    defer { discard write_text("cleanup|") }
    if mode == "final" {
        let values = [0]
        values[1] = rhs()
    } else { if mode == "nested" {
        let matrix = [[0]]
        matrix[1][0] = rhs()
    } else { if mode == "negative" {
        let values = [0]
        discard values[-1]
    } else {
        let bytes = new()
        push(bytes, 0)
        if mode == "bytes-bound" { discard bytes[1] } else {
            let value = if mode == "byte-negative" { -1 } else { 256 }
            bytes[0] = value
        }
    } } }
}
"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "index-fault");
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
        for (mode, expected, diagnostic) in [
            ("final", "rhs|cleanup|", "list index out of bounds"),
            ("nested", "cleanup|", "list index out of bounds"),
            ("negative", "cleanup|", "list index out of bounds"),
            ("bytes-bound", "cleanup|", "bytes index out of bounds"),
            ("byte-negative", "cleanup|", "byte value out of range"),
            ("byte-large", "cleanup|", "byte value out of range"),
        ] {
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
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(diagnostic),
                "{output:?}"
            );
        }
    }
}
