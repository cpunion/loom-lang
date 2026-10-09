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

#[test]
fn cached_method_guarantees_recheck_weakened_result_types() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("main.loom");
    let original = r#"
type Positive = Int where self > 0
type AsyncPositive = Int where self > 0
concept Source {
    fn read(self Self) Positive
    async fn wait(self Self) AsyncPositive
}
impl Source for Bool {
    fn read(self Bool) Positive {
        Positive(7)
    }
    async fn wait(self Bool) AsyncPositive {
        AsyncPositive(7)
    }
}
fn generic[T Source](source T) Int
ensures result > 0 {
    source.read()
}
async fn dynamic(source dyn Source) Int
ensures result > 0 {
    source.wait().await
}
async fn main() {
    assert generic(true) == 7
    let source dyn Source = false
    assert dynamic(source).await == 7
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
    // Both implementations still return 7. A private summary must nevertheless
    // forget the stronger type invariant, rather than guessing those witnesses.
    for name in ["Positive", "AsyncPositive"] {
        let from = format!("type {name} = Int where self > 0");
        let to = format!("type {name} = Int where self >= 0");
        let weakened = original.replacen(&from, &to, 1);
        fs::write(&source, weakened).unwrap();
        let rejected = check();
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("postcondition"));
        fs::write(&source, original).unwrap();
        success(&check());
    }
}

#[test]
fn cached_task_factory_proofs_recheck_changed_bodies() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("main.loom");
    let original = r#"
async fn keep(value Int) Int
ensures result == value {
    value
}
fn make(value Int) Task[Int] {
    keep(value)
}
async fn forward(value Int) Int
ensures result == value {
    make(value).await
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
    // Only the uncontracted factory body changes. The async producer's proved
    // contract is still true, but cannot validate the cached caller anymore.
    let changed = original.replacen("    keep(value)\n", "    keep(value + 1)\n", 1);
    fs::write(&source, changed).unwrap();
    let rejected = check();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("postcondition"));
    fs::write(&source, original).unwrap();
    success(&check());
}
