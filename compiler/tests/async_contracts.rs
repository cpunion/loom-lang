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
fn cached_async_body_inference_rechecks_uncontracted_producers() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("main.loom");
    let original = r#"
import std.time.sleep_ms
import std.task.all
import std.task.settled
import std.task.Outcome
import std.list.length
fn task_count[T](values List[T]) Int {
    length(values)
}
fn forwarded(tasks List[Task[Int]]) List[Task[Int]]
ensures length(result) == old(task_count(tasks)) {
    tasks
}
async fn keep(value Int) Int {
    sleep_ms(1).await
    value
}
async fn forward(value Int) Int
ensures result == value {
    keep(value).await
}
async fn combined(value Int) Int
ensures result == value {
    let first, second = all((keep(value), keep(value))).await
    assert second == value
    first
}
async fn collected(value Int) Int
ensures result == value {
    let first, second = settled((keep(value), keep(value))).await
    discard second
    match first {
        Outcome.Completed(number) => number
        _ => value
    }
}
async fn main() {
    assert forward(42).await == 42
    assert combined(43).await == 43
    assert collected(44).await == 44
    let values = all(forwarded([keep(45), keep(46)])).await
    assert length(values) == 2
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

    let changed = original.replacen("    length(values)\n}", "    length(values) + 1\n}", 1);
    assert_ne!(changed, original);
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

#[test]
fn cached_enum_factory_proofs_recheck_selected_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("main.loom");
    let original = r#"
async fn keep(value Int) Int
ensures result == value {
    value
}
enum Work {
    Pending(Task[Int])
    Ready(Int)
}
fn make(flag Bool, value Int) Work {
    if flag {
        Work.Pending(keep(value))
    } else {
        Work.Ready(value)
    }
}
async fn forward(flag Bool, value Int) Int
ensures result == value {
    match make(flag, value) {
        Work.Pending(task) => task.await
        Work.Ready(number) => number
    }
}
async fn main() {
    assert forward(true, 42).await == 42
    assert forward(false, 43).await == 43
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
    // Both arms must be rechecked. A known async target supplies no guarantee
    // for a changed input, and a Ready payload cannot inherit that target.
    for (from, to) in [
        ("keep(value)", "keep(value + 1)"),
        ("Work.Ready(value)", "Work.Ready(value + 1)"),
    ] {
        fs::write(&source, original.replacen(from, to, 1)).unwrap();
        let rejected = check();
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("postcondition"));
        fs::write(&source, original).unwrap();
        success(&check());
    }
}
