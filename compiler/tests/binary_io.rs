use std::{fs, process::Command};
mod common;
use common::success;

// This private fixture exercises the new compiler mechanisms before the source
// standard library adopts them through a verified bootstrap checkpoint.
const RAW: &str = r#"
intrinsic fn bytes_new() Bytes
intrinsic fn bytes_len(value Bytes) Int
intrinsic fn bytes_push(value Bytes, byte Int)
intrinsic fn bytes_get(value Bytes, index Int) Int
intrinsic fn bytes_set(value Bytes, index Int, byte Int)
intrinsic fn bytes_text_copy(value Bytes) Text
intrinsic fn create(path Text) Int
intrinsic fn open(path Text) Int
intrinsic fn read(handle Int, value Bytes, limit Int) Int
intrinsic fn write_bytes(handle Int, value Bytes, offset Int) Int
intrinsic fn close(handle Int) Int
intrinsic fn arg_text(index Int) Text
pub fn new() Bytes { bytes_new() }
pub fn length(value Bytes) Int { bytes_len(value) }
pub fn push(value Bytes, byte Int) { bytes_push(value, byte) }
pub fn get(value Bytes, index Int) Int { bytes_get(value, index) }
pub fn set(value Bytes, index Int, byte Int) { bytes_set(value, index, byte) }
pub fn text(value Bytes) Text { bytes_text_copy(value) }
pub fn argument() Text { arg_text(1) }
pub fn roundtrip(path Text, value Bytes) Bytes {
    let output = create(path)
    assert output >= 0
    var offset = 0
    while offset < length(value) {
        let count = write_bytes(output, value, offset)
        assert count > 0
        offset = offset + count
    }
    assert close(output) == 0
    let input = open(path)
    assert input >= 0
    let copied = new()
    var count = read(input, copied, 31)
    while count > 0 { count = read(input, copied, 31) }
    assert count == 0 && close(input) == 0
    copied
}
"#;

const PROGRAM: &str = r#"
import std.raw.new
import std.raw.length
import std.raw.push
import std.raw.get
import std.raw.set
import std.raw.text
import std.raw.argument
import std.raw.roundtrip
fn grow(value Bytes, result_value Int) Int {
    var index = 0
    while index < 256 { push(value, index)
        index = index + 1 }
    result_value
}
fn captured() (Bytes, Bytes) {
    let value = new()
    push(value, 128)
    let alias = value
    set(alias, 0, 255)
    assert get(value, 0) == 255
    (value, alias)
}
fn main() {
    let value = new()
    push(value, 65)
    let alias = value
    let saved = text(value)
    assert get(value, grow(alias, 0)) == 65
    set(value, grow(alias, 0), grow(alias, 255))
    assert get(alias, 0) == 255 && saved == "A"
    let first, second = comptime { captured() }
    set(first, 0, 128)
    assert get(second, 0) == 128
    let copied = roundtrip(argument(), value)
    assert length(copied) == length(value)
    var index = 0
    while index < length(value) { assert get(copied, index) == get(value, index)
        index = index + 1 }
}
"#;

#[test]
fn native_bytes_keep_shared_growth_snapshots_and_binary_file_contents() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let standard = root.join("std");
    fs::create_dir_all(standard.join("raw")).unwrap();
    fs::write(standard.join("raw/raw.loom"), RAW).unwrap();
    let package = root.join("app");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("main.loom"), PROGRAM).unwrap();
    let executable = common::executable(root, "binary");
    let ir = root.join("binary.ll");
    let file = root.join("bytes-é.bin");
    for level in ["0", "2"] {
        success(
            &Command::new(common::compiler())
                .arg("build")
                .arg(&package)
                .arg("--std")
                .arg(&standard)
                .args(["--native-tool", env!("CARGO_BIN_EXE_loom-native")])
                .arg("--output")
                .arg(&executable)
                .arg("--emit-ir")
                .arg(&ir)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &Command::new(&executable)
                .arg(&file)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        let mut expected = vec![255];
        for _ in 0..3 {
            expected.extend(0..=255u8);
        }
        assert_eq!(fs::read(&file).unwrap(), expected);
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(ir.contains("load i8") && ir.contains("store i8"));
        assert!(!ir.contains("loom_rt_bytes_get") && !ir.contains("loom_rt_bytes_set"));
    }
}
