use std::fs;

mod common;
use common::success;

#[test]
fn byte_constraints_preserve_shared_storage_through_helpers_faults_and_waits() {
    let package = "compiler/examples/byte_constraints";
    for mode in ["check", "test", "run"] {
        success(&common::loom(&[mode, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let tests = common::executable(temporary.path(), "byte-constraint-tests");
    let app = common::executable(temporary.path(), "byte-constraints");
    for level in ["0", "2"] {
        success(
            &common::command(&["test", package, "--no-run"])
                .arg("--output")
                .arg(&tests)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&tests));
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&app)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&app));
    }
    fs::write(
        temporary.path().join("main.loom"),
        r#"
import std.bytes.length

type Head = Bytes where length(self) > 0 && self[0] > 0

fn temporarily_invalid(value Bytes) {
    value[0] = 0
    value[0] = 7
}

pub fn invalid(value Head) {
    temporarily_invalid(value)
}

fn main() {
}
"#,
    )
    .unwrap();
    let rejected = common::loom(&["check", temporary.path().to_str().unwrap()]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("constrained sequence"));

    for condition in ["result == old(value[0])", "result == value[0]"] {
        fs::write(
            temporary.path().join("main.loom"),
            format!(
                r#"
import std.bytes.length
import std.bytes.push
import std.task.worker.run

type Head = Bytes where length(self) > 0 && self[0] > 0

pub fn replace(value Head) {{
    value[0] = 7
    push(value, 0)
}}

pub fn sampled(value Head) Int
ensures {condition}
{{
    value[0]
}}

async fn main() {{
    discard run(fn() Int {{
            1
        }}).await
}}
"#,
            ),
        )
        .unwrap();
        let rejected = common::loom(&["check", temporary.path().to_str().unwrap()]);
        assert_eq!(rejected.status.code(), Some(1), "accepted {condition}");
        let message = String::from_utf8_lossy(&rejected.stderr);
        assert!(message.contains("interference-safe"), "{message}");
    }
}
