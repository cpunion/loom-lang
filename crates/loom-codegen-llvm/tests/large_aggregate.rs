use std::path::Path;
use std::process::Command;

use loom_codegen_llvm::EmitOptions;
use loom_driver::AnalysisHost;

mod support;
use support::emit_native;

const CHILD_PROJECT_ENV: &str = "LOOM_LARGE_AGGREGATE_INTERPRETER_CHILD";
const ELEMENT_COUNT: usize = 50_001;

fn snapshot(project: &Path) -> loom_driver::AnalysisSnapshot {
    AnalysisHost::new(project)
        .expect("load large aggregate project")
        .snapshot()
        .expect("analyze large aggregate project")
}

#[test]
fn interpreter_large_aggregate_child() {
    let Ok(project) = std::env::var(CHILD_PROJECT_ENV) else {
        return;
    };
    let snapshot = snapshot(Path::new(&project));
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("run interpreter regression");
    assert_eq!(results.len(), 1, "{results:#?}");
    assert!(
        results.iter().all(|result| result.failure.is_none()),
        "{results:#?}"
    );
}

#[test]
fn copying_and_comparing_a_large_list_is_stack_bounded_on_both_backends() {
    let project = tempfile::tempdir().expect("create large aggregate project");
    let elements = std::iter::repeat_n("0", ELEMENT_COUNT)
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        r"module large_aggregate

fn verify(left List[Int], right List[Int]) {{
    assert left == right
}}

test fn copy_and_compare_large_list() {{
    let values = [{elements}]
    let copied = values
    verify(values, copied)
}}
"
    );
    std::fs::write(project.path().join("main.loom"), source).expect("write regression source");

    let interpreter = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "interpreter_large_aggregate_child",
            "--nocapture",
        ])
        .env(CHILD_PROJECT_ENV, project.path())
        .output()
        .expect("run interpreter regression child");
    assert!(
        interpreter.status.success(),
        "status={:?} stdout={} stderr={}",
        interpreter.status,
        String::from_utf8_lossy(&interpreter.stdout),
        String::from_utf8_lossy(&interpreter.stderr),
    );

    let snapshot = snapshot(project.path());
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let executable = project.path().join("native-tests");
    emit_native(
        snapshot.executable().expect("lower large aggregate MIR"),
        &executable,
        &EmitOptions::tests(),
    )
    .expect("emit native regression executable");
    let native = Command::new(executable)
        .output()
        .expect("run native regression executable");
    assert!(
        native.status.success(),
        "status={:?} stdout={} stderr={}",
        native.status,
        String::from_utf8_lossy(&native.stdout),
        String::from_utf8_lossy(&native.stderr),
    );
    assert_eq!(
        String::from_utf8(native.stdout).expect("native stdout is UTF-8"),
        "passed large_aggregate.copy_and_compare_large_list\n",
    );
}
