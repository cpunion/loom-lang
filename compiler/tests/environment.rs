use std::{fs, process::Command};
mod common;

#[test]
fn source_environment_reads_distinguish_empty_absent_and_invalid_encoding() {
    let temporary = tempfile::tempdir().unwrap();
    let expected = "value=雪🙂".repeat(100);
    fs::write(
        temporary.path().join("main.loom"),
        format!(
            r#"
import std.env.get
import std.env.EnvError
import std.option.Option
import std.result.Result
import std.process.arguments
import std.list.length

fn value(name Text) Text {{
    match get(name) {{
        Result.Ok(found) => match found {{
            Option.Some(text) => text
            Option.None => {{ assert false
                "" }}
        }}
        Result.Err(_) => {{ assert false
            "" }}
    }}
}}
fn main() {{
    if length(arguments()) > 1 {{
        assert match get("LOOM_SOURCE_ENV_INVALID") {{
            Result.Err(error) => error == EnvError.Utf8
            _ => false
        }}
        return
    }}
    let saved = value("LOOM_SOURCE_ENV_VALUE")
    assert value("LOOM_SOURCE_ENV_EMPTY") == ""
    assert match get("LOOM_SOURCE_ENV_ABSENT") {{
        Result.Ok(found) => match found {{ Option.None => true, _ => false }}
        Result.Err(_) => false
    }}
    assert value("LOOM_SOURCE_ENV_VALUE") == saved && saved == {expected:?}
}}
"#
        ),
    )
    .unwrap();
    let binary = common::executable(temporary.path(), "environment");
    common::success(&common::loom(&[
        "build",
        temporary.path().to_str().unwrap(),
        "--output",
        binary.to_str().unwrap(),
    ]));
    let output = Command::new(&binary)
        .env("LOOM_GC_STRESS", "1")
        .env("LOOM_SOURCE_ENV_VALUE", &expected)
        .env("LOOM_SOURCE_ENV_EMPTY", "")
        .env_remove("LOOM_SOURCE_ENV_ABSENT")
        .output()
        .unwrap();
    common::success(&output);
    assert!(output.stdout.is_empty() && output.stderr.is_empty());

    #[cfg(any(unix, windows))]
    {
        use std::ffi::OsString;
        #[cfg(unix)]
        let invalid = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![0xff])
        };
        #[cfg(windows)]
        let invalid = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[0xd800])
        };
        let output = Command::new(binary)
            .arg("invalid-encoding")
            .env("LOOM_GC_STRESS", "1")
            .env("LOOM_SOURCE_ENV_INVALID", invalid)
            .output()
            .unwrap();
        common::success(&output);
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
    }
}
