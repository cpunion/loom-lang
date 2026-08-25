use std::process::Command;

use loom_codegen_llvm::{EmitOptions, emit_native};
use loom_driver::AnalysisHost;
use loom_interpreter::{Interpreter, TestStatus, Value};

#[test]
fn explicit_discard_evaluates_values_on_both_backends() {
    let project = tempfile::tempdir().expect("create discard project");
    std::fs::write(
        project.path().join("main.loom"),
        r"module discard_values

record Counter {
    value Int
}

enum DiscardChoice {
    Async,
    Sync
}

impl Counter {
    method next(mut self) Int {
        self.value = self.value + 1
        self.value
    }
}

fn answer() Int {
    42
}

async fn asynchronous_answer() Int {
    Task.sleep(1).await
    answer()
}

pub fn main() {
    var counter = Counter { value = 0 }
    discard answer()
    discard counter.next()
    let observed = counter.value
    assert observed == 1
}

test fn synchronous_discard() {
    var counter = Counter { value = 0 }
    discard counter.next()
    discard answer()
    let observed = counter.value
    assert observed == 1
}

test async fn awaited_discard() {
    discard asynchronous_answer().await
}

test async fn nested_control_discard() {
    discard {
        match DiscardChoice.Async {
            DiscardChoice.Async => if answer() == 42 {
                asynchronous_answer().await
            } else {
                answer()
            }
            DiscardChoice.Sync => answer()
        }
    }
}
",
    )
    .expect("write discard source");

    let snapshot = AnalysisHost::new(project.path())
        .expect("open discard project")
        .snapshot()
        .expect("compile discard project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower discard MIR");

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
    assert_eq!(interpreted_tests.len(), 3, "{interpreted_tests:#?}");
    assert!(
        interpreted_tests
            .iter()
            .all(|result| result.status == TestStatus::Passed),
        "{interpreted_tests:#?}"
    );

    let executable = project.path().join("native-main");
    emit_native(program, &executable, &EmitOptions::run("main")).expect("emit native discard main");
    let native_main = Command::new(&executable)
        .output()
        .expect("run native discard main");
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
        .expect("emit native discard tests");
    let native_tests = Command::new(&test_executable)
        .output()
        .expect("run native discard tests");
    assert!(
        native_tests.status.success(),
        "status={:?} stdout={} stderr={}",
        native_tests.status,
        String::from_utf8_lossy(&native_tests.stdout),
        String::from_utf8_lossy(&native_tests.stderr)
    );
    let stdout = String::from_utf8(native_tests.stdout).expect("native output is UTF-8");
    assert!(
        stdout.contains("passed discard_values.synchronous_discard\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("passed discard_values.awaited_discard\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("passed discard_values.nested_control_discard\n"),
        "{stdout}"
    );
}
