use std::{fs, process::Command};
mod common;
use common::success;

const PROGRAM: &str = r#"
import std.bytes.new
import std.bytes.length
import std.bytes.push
import std.bytes.get
import std.bytes.set
import std.bytes.to_text
import std.list.get
import std.file.read_bytes
import std.file.write_bytes
import std.file.read_text
import std.file.FileError
import std.process.arguments
import std.result.Result
import std.text.concat
fn grow(value Bytes, result_value Int) Int {
    var index = 0
    while index < 4096 { push(value, index % 256)
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
    let saved = to_text(value)
    assert get(value, grow(alias, 0)) == 65
    set(value, grow(alias, 0), grow(alias, 255))
    assert get(alias, 0) == 255 && saved == "A"
    let first, second = comptime { captured() }
    set(first, 0, 128)
    assert get(second, 0) == 128
    let path = get(arguments(), 1)
    assert match write_bytes(path, value) {
        Result.Ok(count) => count == length(value)
        Result.Err(_) => false
    }
    let copied = match read_bytes(path) {
        Result.Ok(bytes) => bytes
        Result.Err(_) => { assert false
            return }
    }
    assert length(copied) == length(value)
    var index = 0
    while index < length(value) { assert get(copied, index) == get(value, index)
        index = index + 1 }
    assert match read_text(path) {
        Result.Err(error) => error == FileError.Utf8
        Result.Ok(_) => false
    }
    let empty_path = concat(path, ".empty")
    assert match write_bytes(empty_path, new()) { Result.Ok(count) => count == 0, _ => false }
    assert match read_bytes(empty_path) { Result.Ok(bytes) => length(bytes) == 0, _ => false }
    let missing = concat(path, "/missing")
    assert match read_bytes(missing) { Result.Err(error) => error == FileError.Open, _ => false }
    assert match write_bytes(missing, value) { Result.Err(error) => error == FileError.Open, _ => false }
}
"#;

#[test]
fn native_bytes_keep_shared_growth_snapshots_and_binary_file_contents() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let package = root.join("app");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("main.loom"), PROGRAM).unwrap();
    let executable = common::executable(root, "binary");
    let ir = root.join("binary.ll");
    let file = root.join("bytes-é.bin");
    for level in ["0", "2"] {
        success(
            &common::command(&["build"])
                .arg(&package)
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
        for _ in 0..48 {
            expected.extend(0..=255u8);
        }
        assert_eq!(fs::read(&file).unwrap(), expected);
        assert!(fs::read(root.join("bytes-é.bin.empty")).unwrap().is_empty());
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(ir.contains("load i8") && ir.contains("store i8"));
        assert!(!ir.contains("loom_rt_bytes_get") && !ir.contains("loom_rt_bytes_set"));
    }
}
