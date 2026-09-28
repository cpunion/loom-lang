use std::{fs, process::Command};

mod common;
use common::{loom, success};

#[test]
fn explicit_options_bind_native_results_cache_reuse_and_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("main.loom");
    fs::write(
        &source,
        r#"import std.build.option
import std.io.write_text
import std.option.Option

fn channel() Text {
    option("app.channel", "default")
}

fn main() {
    discard write_text(channel())
}

test fn configured() {
    assert channel() == "first 雪"
    assert comptime { channel() } == channel()
    assert match option("empty") {
        Option.Some(value) => value == ""
        Option.None => false
    }
    assert match option("absent") {
        Option.Some(_) => false
        Option.None => true
    }
}
"#,
    )
    .unwrap();
    let cache = directory.path().join("cache");
    let executable = common::executable(directory.path(), "configured");
    let receipt = directory.path().join("build.receipt");
    let ir = directory.path().join("configured.ll");
    let command = |mode: &str, channel: &str, reversed: bool| {
        let mut command = common::command(&[mode]);
        command.arg(directory.path());
        let channel = format!("app.channel={channel}");
        let options = if reversed {
            ["empty=", channel.as_str()]
        } else {
            [channel.as_str(), "empty="]
        };
        for option in options {
            command.args(["--build-option", option]);
        }
        command.env("app.channel", "ambient is not a build input");
        command
    };
    success(&command("check", "first 雪", false).output().unwrap());
    success(&command("test", "first 雪", false).output().unwrap());
    let run = command("run", "first 雪", false).output().unwrap();
    success(&run);
    assert_eq!(run.stdout, "first 雪".as_bytes());
    for (level, channel, reversed, hit) in [
        ("0", "first 雪", false, false),
        ("2", "first 雪", true, true),
        ("2", "other 雪", false, false),
    ] {
        let build = command("build", channel, reversed)
            .arg("--output")
            .arg(&executable)
            .arg("--receipt")
            .arg(&receipt)
            .arg("--frontend-cache")
            .arg(&cache)
            .env("LOOM_NATIVE_TIMINGS", "1")
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap();
        success(&build);
        assert!(String::from_utf8_lossy(&build.stderr).contains(if hit {
            "frontend hit"
        } else {
            "frontend miss"
        }));
        let output = Command::new(&executable)
            .env("app.channel", "runtime is not a build input either")
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap();
        success(&output);
        assert_eq!(output.stdout, channel.as_bytes());
        let receipt = fs::read_to_string(&receipt).unwrap();
        assert!(receipt.starts_with("loom-build-receipt 3\n"));
        assert!(receipt.contains("option-count 2\noption 6170702e6368616e6e656c "));
        assert!(!receipt.contains(channel));
    }
    success(
        &command("build", "first 雪", false)
            .arg("--output")
            .arg(&executable)
            .arg("--emit-ir")
            .arg(&ir)
            .output()
            .unwrap(),
    );
    let ir = fs::read_to_string(ir).unwrap();
    assert!(!ir.contains("option_present") && !ir.contains("option_text"));
    let duplicate = command("check", "first 雪", false)
        .args(["--build-option", "empty=again"])
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("duplicate build option name"));
    for value in ["missing-equals", "=empty-name", "bad name=value"] {
        assert!(
            !common::command(&["check"])
                .arg(directory.path())
                .args(["--build-option", value])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    success(&loom(&["test", "compiler/examples/build_options"]));
}
