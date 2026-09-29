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
fn choose(flag Bool) type {
    if flag { Int } else { Text }
}
pub fn answer() Int {
    let Selected = comptime { choose(true) }
    let value Selected = 42
    value
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
