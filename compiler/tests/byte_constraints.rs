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

#[test]
fn opaque_identity_stores_revalidate_when_a_worker_enters_the_cached_build() {
    let example = "compiler/examples/byte_constraints/identity";
    for mode in ["check", "test", "run"] {
        success(&common::loom(&[mode, example]));
    }
    let package = tempfile::tempdir().unwrap();
    let cache = package.path().join("cache");
    let binary = common::executable(package.path(), "identity-stores");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", example])
                .arg("--output")
                .arg(&binary)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&binary));
    }
    let declarations = fs::read_to_string(common::root().join(example).join("main.loom"))
        .unwrap()
        .replace("fn main()", "fn exercise()");
    for shared in [false, true, false] {
        let entry = if shared {
            r#"
import std.task.worker.run

async fn main() {
    match checked_text(to_bytes("hé")) {
        Result.Ok(value) => {
            discard run(fn() {
                    unchanged_text(value)
                }).await
        }
        Result.Err(_) => {
            assert false
        }
    }
}
"#
        } else {
            r#"
fn main() {
    exercise()
}
"#
        };
        fs::write(
            package.path().join("main.loom"),
            format!("{declarations}\n{entry}"),
        )
        .unwrap();
        let checked = common::command(&["check", package.path().to_str().unwrap()])
            .arg("--frontend-cache")
            .arg(&cache)
            .output()
            .unwrap();
        assert_eq!(checked.status.success(), !shared, "{checked:?}");
        if shared {
            assert!(String::from_utf8_lossy(&checked.stderr).contains("constrained sequence"));
        } else {
            success(&common::loom(&["run", package.path().to_str().unwrap()]));
        }
    }
}
