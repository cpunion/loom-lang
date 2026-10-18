use std::fs;

mod common;
use common::success;

#[test]
fn synchronous_borrow_forwarding_avoids_an_extra_task_frame_before_llvm_optimization() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "forwarding");
    let ir = directory.path().join("forwarding.ll");
    let mut task_sites = Vec::new();
    for forward in [
        r#"fn forward(value Guard) Task[Int] {
    read(value)
}"#,
        r#"async fn forward(value Guard) Int {
    read(value).await
}"#,
    ] {
        let source = format!(
            r#"
import std.resource.Dispose
import std.resource.MustScope

record Guard {{
    value Int
}}
impl MustScope for Guard {{
}}
impl Dispose for Guard {{
    fn dispose(self Guard) {{
    }}
}}
async fn read(value Guard) Int {{
    value.value
}}
{forward}
async fn main() {{
    scoped value = Guard {{ value = 7 }}
    assert forward(value).await == 7
}}
"#
        );
        fs::write(directory.path().join("main.loom"), source).unwrap();
        success(
            &common::command(&["build", directory.path().to_str().unwrap()])
                .arg("--output")
                .arg(&executable)
                .arg("--emit-ir")
                .arg(&ir)
                .env("LOOM_OPT_LEVEL", "0")
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&executable));
        task_sites.push(
            fs::read_to_string(&ir)
                .unwrap()
                .lines()
                .filter(|line| line.contains("call i64 @loom_rt_task_create("))
                .count(),
        );
    }
    assert_eq!(task_sites[0] + 1, task_sites[1]);
}

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
