use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn generated_declarations_use_the_native_package_and_test_pipeline() {
    let example = common::root().join("compiler/examples/declaration_generation");
    let original = fs::read(example.join("main.loom")).unwrap();
    success(&loom(&["check", example.to_str().unwrap()]));
    for level in ["0", "2"] {
        let output = common::command(&["test", example.to_str().unwrap()])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap();
        success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("3 tests passed"));
        success(
            &Command::new(common::executable(&example.join("target"), "tests"))
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
    success(&loom(&["run", example.to_str().unwrap()]));
    assert_eq!(fs::read(example.join("main.loom")).unwrap(), original);
}

#[test]
fn declaration_stages_are_pure_and_generated_code_is_not_trusted() {
    let folder = tempfile::tempdir().unwrap();
    for source in [
        r#"
import std.io.write_text
fn generate() Text {
    discard write_text("generation-side-effect")
    ""
}
comptime {
    generate()
}
"#,
        r#"
comptime {
    """
    fn wrong() Int
    ensures result == 1
    {
        2
    }
    """
}
"#,
        r#"
record Item {
    first Int
}
comptime {
    """
    record Item {
        second Text
    }
    """
}
"#,
        r#"
comptime {
    "import std.io.write_text"
}
"#,
        r#"
comptime {
    42
}
"#,
    ] {
        fs::write(folder.path().join("main.loom"), source).unwrap();
        let output = loom(&["check", folder.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("generation-side-effect"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("assertion failed"));
    }
}
