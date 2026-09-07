use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn capture_options_change_only_the_child_directory_and_environment() {
    let directory = tempfile::tempdir().unwrap();
    let child_source = directory.path().join("child");
    let parent_source = directory.path().join("parent");
    let working = directory.path().join("child working 雪");
    for path in [&child_source, &parent_source, &working] {
        fs::create_dir(path).unwrap();
    }
    fs::write(directory.path().join("marker"), "parent").unwrap();
    fs::write(working.join("marker"), "child").unwrap();
    fs::write(
        child_source.join("main.loom"),
        r#"
import std.env.get
import std.option.Option
import std.result.Result
import std.file.read_text
import std.process.arguments
import std.process.exit_code
import std.list.get
import std.io.write_text
import std.io.write_error
fn has(name Text, expected Text) Bool {
    match std.env.get(name) { Result.Ok(found) => match found { Option.Some(value) => value == expected, _ => false }, _ => false }
}
fn absent(name Text) Bool {
    match std.env.get(name) { Result.Ok(found) => match found { Option.None => true, _ => false }, _ => false }
}
fn main() {
    let mode = std.list.get(arguments(), 1)
    assert match read_text("marker") { Result.Ok(value) => value == if mode == "default" { "parent" } else { "child" }, _ => false }
    assert if mode == "clear" { absent("LOOM_CAPTURE_PARENT") } else { has("LOOM_CAPTURE_PARENT", "inherited") }
    if mode == "default" {
        assert has("LOOM_CAPTURE_REMOVE", "remove me")
        assert has("LOOM_CAPTURE_VALUE", "original")
    } else {
        assert absent("LOOM_CAPTURE_REMOVE")
        assert has("LOOM_CAPTURE_VALUE", "last=雪")
        assert has("LOOM_CAPTURE_EMPTY", "")
    }
    discard write_text("out\0雪")
    discard write_error("err\0雪")
    exit_code(7)
}
"#,
    )
    .unwrap();
    let child = common::executable(directory.path(), "configured child");
    success(&loom(&[
        "build",
        child_source.to_str().unwrap(),
        "--output",
        child.to_str().unwrap(),
    ]));
    fs::write(
        parent_source.join("main.loom"),
        format!(
            r#"
import std.process.capture
import std.process.Options
import std.process.EnvChange
import std.process.Output
import std.process.SpawnError
import std.process.ExitStatus
import std.env.get
import std.file.read_text
import std.option.Option
import std.result.Result
import std.list.new
import std.list.push
import std.list.set
import std.bytes.to_text
fn check(output Result[Output, SpawnError]) {{
    match output {{
        Result.Err(_) => {{ assert false }}
        Result.Ok(found) => {{
            assert match found.status {{ ExitStatus.Exited(code) => code == 7, _ => false }}
            assert to_text(found.stdout) == "out\0雪"
            assert to_text(found.stderr) == "err\0雪"
        }}
    }}
}}
fn has(name Text, expected Text) Bool {{
    match get(name) {{ Result.Ok(found) => match found {{ Option.Some(value) => value == expected, _ => false }}, _ => false }}
}}
fn main() {{
    let args = new[Text]()
    push(args, {child:?})
    push(args, "default")
    check(capture(args))
    let changes = new[EnvChange]()
    push(changes, EnvChange.Set("LOOM_CAPTURE_VALUE", "first"))
    push(changes, EnvChange.Remove("LOOM_CAPTURE_VALUE"))
    push(changes, EnvChange.Set("LOOM_CAPTURE_VALUE", "last=雪"))
    push(changes, EnvChange.Set("LOOM_CAPTURE_EMPTY", ""))
    push(changes, EnvChange.Remove("LOOM_CAPTURE_REMOVE"))
    push(changes, EnvChange.Set("LOOM_GC_STRESS", "1"))
    match get("SystemRoot") {{
        Result.Ok(found) => {{ match found {{
            Option.Some(value) => {{ push(changes, EnvChange.Set("SystemRoot", value)) }}
            _ => {{}}
        }} }}
        _ => {{}}
    }}
    set(args, 1, "inherit")
    check(capture(args, Options {{ directory = Option.Some({working:?}) clear_environment = false environment = changes }}))
    set(args, 1, "clear")
    check(capture(args, Options {{ directory = Option.Some({working:?}) clear_environment = true environment = changes }}))
    assert has("LOOM_CAPTURE_PARENT", "inherited")
    assert has("LOOM_CAPTURE_REMOVE", "remove me")
    assert has("LOOM_CAPTURE_VALUE", "original")
    assert match read_text("marker") {{ Result.Ok(value) => value == "parent", _ => false }}
}}
"#,
            child = child.to_str().unwrap(),
            working = working.to_str().unwrap(),
        ),
    )
    .unwrap();
    let parent = common::executable(directory.path(), "capture parent");
    success(&loom(&[
        "build",
        parent_source.to_str().unwrap(),
        "--output",
        parent.to_str().unwrap(),
    ]));
    let output = Command::new(parent)
        .current_dir(directory.path())
        .env("LOOM_GC_STRESS", "1")
        .env("LOOM_CAPTURE_PARENT", "inherited")
        .env("LOOM_CAPTURE_REMOVE", "remove me")
        .env("LOOM_CAPTURE_VALUE", "original")
        .output()
        .unwrap();
    success(&output);
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}
