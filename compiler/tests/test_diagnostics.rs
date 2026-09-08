use std::{fs, process::Command};
mod common;
use common::success;

fn diagnostic_text(bytes: &[u8]) -> String {
    // The Windows CRT uses CRLF for stderr; source coordinates and UTF-8 stay exact.
    std::str::from_utf8(bytes).unwrap().replace("\r\n", "\n")
}

#[test]
fn test_failures_keep_names_and_helper_locations_without_sources() {
    let temporary = tempfile::tempdir().unwrap();
    let package = temporary.path().join("package");
    let unavailable = temporary.path().join("sources-moved-away");
    let other_cwd = temporary.path().join("empty");
    fs::create_dir(&package).unwrap();
    fs::create_dir(&other_cwd).unwrap();
    fs::write(
        package.join("loom.toml"),
        "[module]\nname='diagnostic_trial'\n",
    )
    .unwrap();

    // CRLF and a supplementary-plane letter distinguish scalar columns from
    // both UTF-8 byte offsets and UTF-16 code units.
    let helper_source = "fn secondary_fault() { assert false }\r\n\
                         fn helper(订单𐐀 Bool) { assert 订单𐐀 }\r\n";
    let helper = package.join("帮助.loom");
    fs::write(&helper, helper_source).unwrap();
    fs::write(
        package.join("failure_test.loom"),
        r#"import std.io.write_text

test fn 订单失败() {
    defer { discard write_text("outer-cleanup\n") }
    defer {
        discard write_text("inner-cleanup\n")
        secondary_fault()
    }
    helper(false)
}
"#,
    )
    .unwrap();
    let prefix = helper_source
        .lines()
        .nth(1)
        .unwrap()
        .split("assert")
        .next()
        .unwrap();
    let expected = format!(
        "FAIL diagnostic_trial.订单失败\nRuntimeFault: {}:2:{}: assertion failed\n",
        helper.canonicalize().unwrap().display(),
        prefix.chars().count() + 1,
    );
    let executable = common::executable(temporary.path(), "compiled-tests");
    for level in ["0", "2"] {
        let compiled = common::command(&[
            "test",
            package.to_str().unwrap(),
            "--no-run",
            "--output",
            executable.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", level)
        .output()
        .unwrap();
        success(&compiled);
        assert!(String::from_utf8_lossy(&compiled.stdout).starts_with("compiled 1 tests to "));
        assert!(!String::from_utf8_lossy(&compiled.stdout).contains("cleanup"));
        assert!(compiled.stderr.is_empty(), "{compiled:?}");

        // The executable, not a CLI wrapper or a source lookup at runtime,
        // must retain the original test name and the helper's assertion site.
        fs::rename(&package, &unavailable).unwrap();
        let standalone = Command::new(&executable)
            .current_dir(&other_cwd)
            .output()
            .unwrap();
        fs::rename(&unavailable, &package).unwrap();
        assert_eq!(
            standalone.status.code(),
            Some(1),
            "O{level}: {standalone:?}"
        );
        assert_eq!(standalone.stdout, b"inner-cleanup\nouter-cleanup\n");
        assert_eq!(diagnostic_text(&standalone.stderr), expected, "O{level}");

        let cli = common::command(&["test", package.to_str().unwrap()])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap();
        assert_eq!(cli.status.code(), Some(1), "O{level}: {cli:?}");
        assert_eq!(cli.stdout, standalone.stdout);
        let diagnostic = diagnostic_text(&cli.stderr);
        assert!(diagnostic.starts_with(&expected), "O{level}: {diagnostic}");
        assert_eq!(diagnostic.matches("FAIL ").count(), 1, "{diagnostic}");
        assert_eq!(
            diagnostic.matches("RuntimeFault:").count(),
            1,
            "{diagnostic}"
        );
    }
}
