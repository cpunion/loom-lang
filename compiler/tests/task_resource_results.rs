use std::fs;

mod common;
use common::success;

#[test]
fn resource_results_transfer_or_drain_through_typed_frames_under_moving_gc() {
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "resource-results");
    let ir = temporary.path().join("resource-results.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "test",
                "compiler/examples/cleanup",
                "--no-run",
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&common::run_tasks(&executable));
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(ir.contains("loom_rt_task_result_cleanup_push"));
        assert!(!ir.contains("llvm.coro") && !ir.contains("universal"));
    }
}
