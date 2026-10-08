use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn contract_helpers_prove_postconditions_without_leaking_proof_only_targets() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("main.loom");
    let definitions = r#"
fn positive(value Int) Bool {
    value > 0
}
fn proof_only(value Int) Bool
requires value > 0
{
    if value < 0 {
        return false
    }
    var marker = 91827364
    marker = marker + 1
    marker > 0
}
fn good(value Int) Int
requires positive(value)
ensures proof_only(result)
{
    value
}

type Positive = Int where proof_only(self)

fn after_division(value Int, divisor Int) Positive
requires value > 0
{
    let quotient = value / divisor
    discard quotient
    Positive(value)
}

fn weighted_step(value Int, gate Int, ignored Int) Int
requires gate > 0
ensures result == value + 2 {
    value + 2
}

fn weighted_count(gate Int, divisor Int) Int
ensures result == 6 {
    var cursor = 0
    var total = 0
    while cursor < 3 {
        total = weighted_step(total, gate, gate / divisor)
        cursor = cursor + 1
    }
    total
}
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
        for (body, failure) in [
            (
                "assert good(7) == 7\nassert after_division(7, 1) == 7",
                None,
            ),
            ("discard good(0)", Some("precondition failed")),
            ("discard after_division(7, 0)", Some("division by zero")),
            ("assert weighted_count(7, 1) == 6", None),
            ("discard weighted_count(0, 1)", Some("precondition failed")),
            ("discard weighted_count(7, 0)", Some("division by zero")),
        ] {
            fs::write(&source, format!("{definitions}\nfn main() {{\n{body}\n}}")).unwrap();
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
                !ir.contains("91827364") && !ir.contains("91827365"),
                "ensures-only helper entered runtime reachability"
            );
            let output = Command::new(&artifact).output().unwrap();
            if let Some(failure) = failure {
                assert_eq!(output.status.code(), Some(1));
                assert!(String::from_utf8_lossy(&output.stderr).contains(failure));
            } else {
                success(&output);
            }
        }
    }
    // A proof-only helper edit must invalidate the caller, including after an
    // unrelated edit reused its checked body from the frontend cache.
    let cache = directory.path().join("cache");
    let cached = || {
        common::command(&["check", directory.path().to_str().unwrap()])
            .arg("--frontend-cache")
            .arg(&cache)
            .output()
            .unwrap()
    };
    let valid_source = format!("{definitions}\nfn main() {{ assert good(7) == 7 }}");
    fs::write(&source, &valid_source).unwrap();
    success(&cached());
    fs::write(&source, format!("fn unrelated() {{}}\n{valid_source}")).unwrap();
    success(&cached());
    fs::write(&source, valid_source.replace("marker > 0", "marker < 0")).unwrap();
    let changed = cached();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("postcondition"));
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
