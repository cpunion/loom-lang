mod common;
use common::success;

#[test]
fn source_workers_share_typed_data_and_drain_under_moving_gc() {
    let temporary = tempfile::tempdir().unwrap();
    let example = common::executable(temporary.path(), "workers");
    for level in ["0", "2"] {
        success(
            &common::command(&["test", "compiler/std/task/worker"])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&common::executable(
            &common::root().join("compiler/std/task/worker/target"),
            "tests",
        )));
        success(
            &common::command(&[
                "build",
                "compiler/examples/workers",
                "--output",
                example.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&common::run_tasks(&example));
    }
}

#[test]
fn workers_reject_resource_transfer_and_stale_shared_proofs() {
    let package = tempfile::tempdir().unwrap();
    for body in [
        "async fn main() { let mutex = new()\nscoped held = lock(mutex)\ndiscard run(fn() Int { discard held\n1 }).await }",
        "async fn main() { let mutex = new()\nscoped result = run(fn() Guard { lock(mutex) }).await }",
        "fn unchanged(values List[Int]) Int ensures result == std.list.length(values) { std.list.length(values) }\nasync fn main() { discard run(fn() Int { unchanged([1]) }).await }",
    ] {
        std::fs::write(package.path().join("main.loom"), format!(
            "import std.task.worker.run\nimport std.sync.mutex.new\nimport std.sync.mutex.lock\nimport std.sync.mutex.Guard\nimport std.list.length\n{body}\n"
        )).unwrap();
        let output = common::loom(&["check", package.path().to_str().unwrap()]);
        assert!(!output.status.success(), "accepted: {body}");
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(
            message.contains("scoped")
                || message.contains("NoSuspend")
                || message.contains("MustScope")
                || message.contains("interference-safe"),
            "{message}"
        );
    }
}
