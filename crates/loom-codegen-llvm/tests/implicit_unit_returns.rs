use std::process::Command;

use loom_codegen_llvm::{EmitOptions, emit_native};
use loom_driver::AnalysisHost;
use loom_interpreter::{Interpreter, TestStatus, Value};

#[test]
fn omitted_unit_entries_run_and_test_on_both_backends() {
    let project = tempfile::tempdir().expect("create implicit-Unit project");
    std::fs::write(
        project.path().join("main.loom"),
        r"module implicit_unit

async fn asynchronous() {
    Task.sleep(1).await
}

pub fn main() {}

test fn ordinary() {
    assert true
}

test async fn asynchronousTest() {
    asynchronous().await
}
",
    )
    .expect("write implicit-Unit source");

    let snapshot = AnalysisHost::new(project.path())
        .expect("open implicit-Unit project")
        .snapshot()
        .expect("compile implicit-Unit project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower implicit-Unit MIR");

    let (main, span) = program
        .functions
        .iter()
        .find(|function| function.name.rsplit('.').next() == Some("main"))
        .map(|function| (function.id, function.span))
        .expect("main function");
    let interpreted_main = Interpreter::new(program)
        .invoke(main, Vec::new(), span)
        .expect("run interpreter main");
    assert_eq!(interpreted_main, Value::Unit);
    let interpreted_tests = Interpreter::new(program).run_tests();
    assert_eq!(interpreted_tests.len(), 2, "{interpreted_tests:#?}");
    assert!(
        interpreted_tests
            .iter()
            .all(|result| result.status == TestStatus::Passed),
        "{interpreted_tests:#?}"
    );

    let executable = project.path().join("native-main");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit native main executable");
    let native_main = Command::new(&executable)
        .output()
        .expect("run native main executable");
    assert!(
        native_main.status.success(),
        "status={:?} stdout={} stderr={}",
        native_main.status,
        String::from_utf8_lossy(&native_main.stdout),
        String::from_utf8_lossy(&native_main.stderr)
    );
    assert_eq!(native_main.stdout, b"Unit\n");

    let test_executable = project.path().join("native-tests");
    emit_native(program, &test_executable, &EmitOptions::tests())
        .expect("emit native test executable");
    let native_tests = Command::new(&test_executable)
        .output()
        .expect("run native test executable");
    assert!(
        native_tests.status.success(),
        "status={:?} stdout={} stderr={}",
        native_tests.status,
        String::from_utf8_lossy(&native_tests.stdout),
        String::from_utf8_lossy(&native_tests.stderr)
    );
    let stdout = String::from_utf8(native_tests.stdout).expect("native test output is UTF-8");
    assert!(
        stdout.contains("passed implicit_unit.ordinary\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("passed implicit_unit.asynchronousTest\n"),
        "{stdout}"
    );
}
