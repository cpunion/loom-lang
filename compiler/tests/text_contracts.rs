use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn text_contracts_prove_without_runtime_postcondition_helpers() {
    let package = "compiler/examples/text_contracts";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "text-contracts");
    let ir = temporary.path().join("text-contracts.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .arg("--emit-ir")
                .arg(&ir)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        let emitted = fs::read_to_string(&ir).unwrap();
        for marker in [
            "91827365", "81726354", "71928364", "61728394", "52849163", "63849271", "74958362",
            "83194627", "41273895", "41373895",
        ] {
            assert!(
                !emitted.contains(marker),
                "proof-only Text helper became a runtime root"
            );
        }
        success(
            &Command::new(&executable)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn text_slice_contracts_reject_false_lengths_and_undefined_utf8_ranges() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    for body in [
        r#"
fn wrong(value Text, start Int, end Int) Text
ensures std.text.length(result) == end - start + 1
{
    std.text.slice(value, start, end)
}
"#,
        r#"
fn undefined(value Text, start Int, end Int) Bool
requires start >= 0 && start <= end && end <= std.text.length(value)
ensures std.text.slice(value, start, end) == std.text.slice(value, start, end)
{
    true
}
"#,
        r#"
fn split_scalar() Bool
ensures std.text.slice("é", 1, 1) == ""
{
    true
}
"#,
    ] {
        fs::write(
            &source,
            format!("import std.text.slice\nimport std.text.length\n{body}"),
        )
        .unwrap();
        let output = common::command(&["check"])
            .arg(temporary.path())
            .output()
            .unwrap();
        assert!(!output.status.success(), "{body}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("postcondition"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::write(
        &source,
        "import std.text.slice\nfn main() {\n    discard slice(\"é\", 1, 1)\n}\n",
    )
    .unwrap();
    let output = common::command(&["run"])
        .arg(temporary.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("invalid text slice range or UTF-8 boundary"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
