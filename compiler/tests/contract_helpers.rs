use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn contract_helpers_prove_postconditions_without_leaking_proof_only_targets() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("main.loom");
    let definitions = r#"
fn positive(value Int) Bool { value > 0 }
fn proof_only(value Int) Bool requires value > 0 {
    let marker = 91827365
    marker > 0
}
fn good(value Int) Int requires positive(value) ensures proof_only(result) { value }
"#;
    let artifact = common::executable(directory.path(), "contracts");
    let ir_path = directory.path().join("contracts.ll");
    let example = common::root().join("compiler/examples/contracts");
    success(&common::loom(&["check", example.to_str().unwrap()]));
    success(&common::loom(&["test", example.to_str().unwrap()]));
    for level in ["0", "2"] {
        success(
            &common::command(&["run", example.to_str().unwrap()])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        for valid in [true, false] {
            fs::write(
                &source,
                format!(
                    "{definitions}\nfn main() {{ assert good({}) == 7 }}",
                    if valid { 7 } else { 0 }
                ),
            )
            .unwrap();
            success(
                &common::command(&[
                    "build",
                    directory.path().to_str().unwrap(),
                    "--output",
                    artifact.to_str().unwrap(),
                    "--emit-ir",
                    ir_path.to_str().unwrap(),
                ])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
            );
            let ir = fs::read_to_string(&ir_path).unwrap();
            assert!(
                !ir.contains("91827365"),
                "ensures-only helper entered runtime reachability"
            );
            let output = Command::new(&artifact).output().unwrap();
            if valid {
                success(&output);
            } else {
                assert_eq!(output.status.code(), Some(1));
                assert!(String::from_utf8_lossy(&output.stderr).contains("precondition failed"));
            }
        }
    }
    fs::write(
        source,
        "fn admitted(value Int) Bool requires value > 0 { true }\nfn bad(value Int) Int ensures admitted(result) { value }\nfn main() { discard bad(7) }",
    )
    .unwrap();
    assert!(
        !common::loom(&["check", directory.path().to_str().unwrap()])
            .status
            .success()
    );
}
