use std::fs;

mod common;
use common::success;

#[test]
fn quantified_domains_compose_without_granting_stronger_types_or_accesses() {
    let package = "compiler/examples/sequence_domains";
    for mode in ["check", "test", "run"] {
        success(&common::loom(&[mode, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let binary = common::executable(temporary.path(), "sequence-domains");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&binary)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&binary));
    }
    let source = fs::read_to_string(common::root().join(package).join("main.loom")).unwrap();
    for invalid in [
        source.replace("values[index] > 255", "values[index] > 254"),
        source.replace("values[index] < 0", "values[index] <= 0"),
        source.replace(
            "values[index] < -9223372036854775808 || values[index] > 9223372036854775807",
            "values[index] < 0 || values[index] > 255",
        ),
        source.replace(
            "values[0] = -9223372036854775808",
            "values[0] = -9223372036854775807",
        ),
        source.replace(
            "ensures result && machine_range(values)",
            "ensures values[length(values)] >= -9223372036854775808 && result && machine_range(values)",
        ),
    ] {
        fs::write(temporary.path().join("main.loom"), invalid).unwrap();
        let output = common::loom(&["check", temporary.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(
            message.contains("not proved") || message.contains("constrained sequence"),
            "{message}"
        );
    }
}
