use std::fs;
mod common;
use common::{loom, success};

#[test]
fn async_guarantees_compose_through_saved_native_tasks() {
    let package = "compiler/examples/async_contracts";
    for command in ["check", "test", "run"] {
        success(&loom(&[command, package]));
    }
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "async-contracts");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package, "--output", executable.to_str().unwrap()])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&executable));
    }
}

#[test]
fn cached_async_guarantees_recheck_changed_producer_bodies() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("main.loom");
    let original = r#"
async fn keep(value Int) Int
ensures result == value {
    value
}
async fn forward(value Int) Int
ensures result == value {
    let saved = keep(value)
    saved.await
}
async fn main() {
    assert forward(42).await == 42
}
"#;
    fs::write(&source, original).unwrap();
    let package = directory.path().to_str().unwrap();
    let cache = directory.path().join("cache");
    let check = || {
        loom(&[
            "check",
            package,
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    for _ in 0..2 {
        success(&check());
    }
    let changed = original.replacen("    value\n}", "    value + 1\n}", 1);
    fs::write(&source, changed).unwrap();
    let rejected = check();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("postcondition"));
    fs::write(&source, original).unwrap();
    success(&check());
}
