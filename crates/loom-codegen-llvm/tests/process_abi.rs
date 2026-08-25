use std::process::Command;

use loom_codegen_llvm::{EmitOptions, emit_native};
use loom_driver::AnalysisHost;

#[test]
fn process_arguments_declaration_matches_the_runtime_abi() {
    let source = r#"module process_abi

import standard.process.arguments

pub fn main() Unit {
    let values = arguments()
    let length = values.length()
    assert length == 2
    match values.get(0) {
        Some(value) => {
            assert value == "alpha"
            Unit
        }
        None => {
            assert false
            Unit
        }
    }
    Unit
}
"#;
    let project = tempfile::tempdir().expect("create process ABI project");
    std::fs::write(project.path().join("main.loom"), source).expect("write process ABI source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load process ABI project")
        .snapshot()
        .expect("analyze process ABI project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());

    let executable = project.path().join("program");
    emit_native(
        snapshot.executable().expect("lower process ABI MIR"),
        &executable,
        &EmitOptions::run("main"),
    )
    .expect("emit process ABI executable");
    let output = Command::new(executable)
        .args(["alpha", "beta"])
        .output()
        .expect("run process ABI executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
}
