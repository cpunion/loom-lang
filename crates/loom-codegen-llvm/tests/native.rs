#![allow(clippy::default_trait_access)]

use std::collections::BTreeMap;
use std::process::Command;

use loom_codegen_llvm::{EmitOptions, emit_native};
use loom_driver::AnalysisHost;
use loom_mir::{Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, Program, Type};

#[test]
fn emits_links_and_runs_a_native_unit_entry() {
    let mut program = Program::default();
    program.functions.push(Function {
        id: FunctionId(0),
        name: "sample.main".into(),
        span: Default::default(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                kind: ExprKind::Constant(Constant::Unit),
                ty: Type::Unit,
                span: Default::default(),
            })),
            span: Default::default(),
        },
        call_plan: CallPlan::default(),
    });
    program.exports = BTreeMap::from([("main".into(), FunctionId(0))]);
    let directory = tempfile::tempdir().expect("create temp directory");
    let executable = directory.path().join("program");
    let artifact = emit_native(&program, &executable, &EmitOptions::run("main"))
        .expect("emit native executable");
    assert_eq!(artifact.functions, 1);
    let output = Command::new(&executable).output().expect("run executable");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
}

#[test]
fn core_examples_compile_and_run_as_native_programs() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    for version in ["core01", "core02", "core03"] {
        let source = workspace.join("examples").join(version);
        let snapshot = AnalysisHost::new(&source)
            .expect("load project")
            .snapshot()
            .expect("analyze project");
        assert!(
            !snapshot.has_errors(),
            "{version}: {:?}",
            snapshot.diagnostics()
        );
        let program = snapshot.executable().expect("lower executable MIR");
        let directory = tempfile::tempdir().expect("create temp directory");
        let executable = directory.path().join("program");
        let mut options = EmitOptions::run("main");
        if version == "core03" {
            options.emit_ir = Some(directory.path().join("program.ll"));
        }
        emit_native(program, &executable, &options)
            .unwrap_or_else(|error| panic!("{version}: {error}"));
        let output = Command::new(&executable).output().expect("run executable");
        assert!(
            output.status.success(),
            "{version}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
        if let Some(ir) = &options.emit_ir {
            let ir = std::fs::read_to_string(ir).expect("read async LLVM IR");
            assert!(ir.contains("@loom.resume."), "{ir}");
            assert!(ir.contains("@loom_executor_run"), "{ir}");
            assert!(ir.contains("@loom_task_suspend_value"), "{ir}");
            assert!(ir.contains("@loom_task_from_wait_source"), "{ir}");
            assert!(ir.contains("@loom_join_create"), "{ir}");
            assert!(ir.contains("@loom_wait_now_ns"), "{ir}");
            assert!(ir.contains("state.resume."), "{ir}");
        }

        let tests = directory.path().join("tests");
        emit_native(program, &tests, &EmitOptions::tests())
            .unwrap_or_else(|error| panic!("{version} tests: {error}"));
        let output = Command::new(&tests).output().expect("run native tests");
        assert!(
            output.status.success(),
            "{version} tests: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.lines().all(|line| line.starts_with("passed ")),
            "{stdout}"
        );
    }
}

#[test]
fn stored_tasks_dynamic_lists_and_join_modes_run_natively() {
    let source = r"module joins

async fn one() Int {
    // Keep the completion ordering deterministic even when the full test
    // workspace is CPU-saturated and both timers are observed in one poll.
    Task.sleep(50).await
    1
}

async fn two() Int {
    Task.sleep(1).await
    2
}

pub async fn main() Unit {
    Task.waitWritable(1).await
    let first = one()
    let second = two()
    let values = Task.all([first, second]).await

    let combined = Task.all(one(), two())
    let left, right = combined.await
    assert left == 1
    assert right == 2

    let winner = Task.any([one(), two()]).await
    assert winner == 2

    let settled = Task.settled([one(), two()])
    let outcomes = settled.await

    let raced = Task.race([one(), two()])
    let outcome = raced.await
    Unit
}
";
    let project = tempfile::tempdir().expect("create join project");
    std::fs::write(project.path().join("main.loom"), source).expect("write join source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load join project")
        .snapshot()
        .expect("analyze join project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower join MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit native join executable");
    let output = Command::new(executable).output().expect("run native joins");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
}

#[test]
fn cancellation_resumes_the_suspended_state_and_runs_cleanup() {
    let source = r"module cancellation

async fn slow() Int {
    defer {
        assert false
    }
    Task.sleep(100).await
    1
}

async fn fast() Int {
    Task.sleep(1).await
    2
}

pub async fn main() Unit {
    let winner = Task.any(slow(), fast()).await
    assert winner == 2
    Unit
}
";
    let project = tempfile::tempdir().expect("create cancellation project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load cancellation project")
        .snapshot()
        .expect("analyze cancellation project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower cancellation MIR");
    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(program, &executable, &options).expect("emit cancellation executable");
    let llvm = std::fs::read_to_string(ir).expect("read cancellation IR");
    assert!(llvm.contains("state.cancelled"), "{llvm}");
    assert!(llvm.contains("AssertionFault"), "{llvm}");
    let output = Command::new(executable)
        .output()
        .expect("run cancellation executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("AssertionFault\n"), "{stdout}");
    assert!(stdout.ends_with("Unit\n"), "{stdout}");
}

#[test]
fn nested_control_await_resumes_in_the_selected_branch() {
    let source = r"module nested

async fn child(value Int) Int {
    Task.sleep(1).await
    value
}

async fn choose(flag Bool) Int {
    if flag {
        child(7).await
    } else {
        child(9).await
    }
}

pub async fn main() Unit {
    let selected = choose(true).await
    assert selected == 7
    Unit
}
";
    let project = tempfile::tempdir().expect("create nested await project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load nested await project")
        .snapshot()
        .expect("analyze nested await project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower nested await MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit nested await executable");
    let output = Command::new(executable)
        .output()
        .expect("run nested await executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
}

#[test]
fn static_generic_concepts_and_conditional_witnesses_compile_natively() {
    let source = r"module sample

concept Equivalent {
    method equivalent(self, other Self) Bool
}

record Atom { value Int }

impl Equivalent for Atom {
    method equivalent(self, other Atom) Bool { self.value == other.value }
}

record Boxed[T] { value T }

impl[T: Equivalent] Equivalent for Boxed[T] {
    method equivalent(self, other Boxed[T]) Bool {
        self.value.equivalent(other.value)
    }
}

fn same[T: Equivalent](left T, right T) Bool {
    left.equivalent(right)
}

pub fn main() Unit {
    let left = Boxed { value = Atom { value = 7 } }
    let right = Boxed { value = Atom { value = 7 } }
    let equal = same(left, right)
    assert equal
    Unit
}

test fn conditional_witness() {
    let left = Boxed { value = Atom { value = 7 } }
    let right = Boxed { value = Atom { value = 7 } }
    let equal = same(left, right)
    assert equal
    Unit
}
";
    let project = tempfile::tempdir().expect("create source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load project")
        .snapshot()
        .expect("analyze project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower executable MIR");

    for (name, options) in [
        ("program", EmitOptions::run("main")),
        ("tests", EmitOptions::tests()),
    ] {
        let executable = project.path().join(name);
        emit_native(program, &executable, &options).expect("emit native executable");
        let output = Command::new(&executable)
            .output()
            .expect("run native executable");
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn conformance_and_method_proofs_keep_their_native_parameter_order() {
    let source = r#"module sample

concept Check {
    method check(self) Bool
}

concept Combine {
    method both[U: Check](self, other U) Bool
}

record Left { value Int }
record Right { value Text }
record Holder[T] { value T }

impl Check for Left {
    method check(self) Bool { self.value == 1 }
}

impl Check for Right {
    method check(self) Bool { self.value == "ok" }
}

impl[T: Check] Combine for Holder[T] {
    method both[U: Check](self, other U) Bool {
        self.value.check() && other.check()
    }
}

pub fn main() Unit {
    let holder = Holder { value = Left { value = 1 } }
    let other = Right { value = "ok" }
    let combined = holder.both(other)
    assert combined
    Unit
}
"#;
    let project = tempfile::tempdir().expect("create source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load project")
        .snapshot()
        .expect("analyze project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower executable MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main")).expect("emit native executable");
    let output = Command::new(&executable)
        .output()
        .expect("run native executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn native_int_is_checked_i64_even_after_llvm_optimization() {
    let cases = [
        (
            "overflow",
            "let value = 9223372036854775807 + 1",
            "IntegerOverflow",
        ),
        (
            "division_by_zero",
            "let value = 7 / 0",
            "IntegerDivisionByZero",
        ),
        (
            "division_overflow",
            "let value = -9223372036854775808 / -1",
            "IntegerDivisionOverflow",
        ),
    ];
    for (name, statement, expected) in cases {
        let source =
            format!("module sample\n\npub fn main() Unit {{\n    {statement}\n    Unit\n}}\n");
        let project = tempfile::tempdir().expect("create source project");
        std::fs::write(project.path().join("main.loom"), source).expect("write source");
        let snapshot = AnalysisHost::new(project.path())
            .expect("load project")
            .snapshot()
            .expect("analyze project");
        assert!(
            !snapshot.has_errors(),
            "{name}: {:?}",
            snapshot.diagnostics()
        );
        let program = snapshot.executable().expect("lower executable MIR");
        let executable = project.path().join("program");
        emit_native(program, &executable, &EmitOptions::run("main"))
            .expect("emit native executable");
        let output = Command::new(&executable)
            .output()
            .expect("run native executable");
        assert!(!output.status.success(), "{name} unexpectedly succeeded");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{name}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn float_text_builtins_compile_and_run_natively() {
    let source = r#"module sample

import standard.float.parse_float
import standard.float.format_float

pub fn main() Unit {
    let finite = format_float(1.25)
    assert finite == "1.25"
    let large = format_float(1e20)
    assert large == "100000000000000000000.0"
    let small = format_float(1e-7)
    assert small == "0.0000001"
    let negative_zero = format_float(-0.0)
    assert negative_zero == "-0.0"
    let positive_infinity = format_float(1.0 / 0.0)
    assert positive_infinity == "Infinity"
    let negative_infinity = format_float(-1.0 / 0.0)
    assert negative_infinity == "-Infinity"
    let not_a_number = format_float(0.0 / 0.0)
    assert not_a_number == "NaN"

    match parse_float("1e3") {
        Ok(value) => {
            assert value == 1000.0
            Unit
        }
        _ => {
            assert false
            Unit
        }
    }
    match parse_float("1") {
        Err(standard.float.ParseFloatError.InvalidSyntax) => Unit
        _ => {
            assert false
            Unit
        }
    }
    match parse_float("1e999") {
        Err(standard.float.ParseFloatError.OutOfRange) => Unit
        _ => {
            assert false
            Unit
        }
    }
    Unit
}

"#;
    let project = tempfile::tempdir().expect("create source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load project")
        .snapshot()
        .expect("analyze project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower executable MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit native executable with float runtime");
    let output = Command::new(&executable)
        .output()
        .expect("run native executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
