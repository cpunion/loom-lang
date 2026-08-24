#![allow(clippy::default_trait_access)]

use std::collections::BTreeMap;
use std::process::Command;

use loom_codegen_llvm::{
    EmitOptions, OptimizationProfile, emit_native, emit_native_object, target_identity,
    validate_native_link_target,
};
use loom_driver::AnalysisHost;
use loom_mir::{Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, Program, Type};

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
const CROSS_TRIPLE: &str = "x86_64-unknown-linux-gnu";
#[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
const CROSS_TRIPLE: &str = "aarch64-unknown-linux-gnu";

#[test]
fn emits_links_and_runs_a_native_unit_entry() {
    let program = unit_program();
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
fn release_and_cross_target_object_policies_are_real_target_inputs() {
    let development =
        target_identity(None, OptimizationProfile::Development).expect("development target");
    let release = target_identity(None, OptimizationProfile::Release).expect("release target");
    assert_eq!(development.triple, release.triple);
    assert_eq!(development.data_layout, release.data_layout);
    assert_ne!(development.optimization, release.optimization);

    let program = unit_program();
    let directory = tempfile::tempdir().expect("create cross target directory");
    let object = directory.path().join("program-aarch64.o");
    let options = EmitOptions::run("main")
        .with_target_triple(Some(CROSS_TRIPLE.to_owned()))
        .with_optimization(OptimizationProfile::Release);
    emit_native_object(&program, &object, &options).expect("emit AArch64 ELF object");
    assert!(
        std::fs::read(&object)
            .expect("read cross object")
            .starts_with(b"\x7fELF")
    );
    let error = validate_native_link_target(&options).expect_err("cross link is unavailable");
    assert_eq!(error.code(), "CrossLinkUnavailable");
}

#[test]
fn release_pipeline_folds_live_constants_and_eliminates_machine_dead_code() {
    let source = r"module optimize

fn folded() Int {
    40 + 2
}

fn unreachable() Int {
    100 + 23
}

pub fn main() Unit {
    let value = folded()
    assert value == 42
    Unit
}
";
    let project = tempfile::tempdir().expect("create optimization project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load project")
        .snapshot()
        .expect("analyze project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower executable MIR");

    let development_ir = project.path().join("development.ll");
    let development_object = project.path().join("development.o");
    let mut development = EmitOptions::run("main");
    development.emit_ir = Some(development_ir.clone());
    emit_native_object(program, &development_object, &development).expect("emit development IR");

    let release_ir = project.path().join("release.ll");
    let release_object = project.path().join("release.o");
    let mut release = EmitOptions::run("main").with_optimization(OptimizationProfile::Release);
    release.emit_ir = Some(release_ir.clone());
    emit_native_object(program, &release_object, &release).expect("emit release IR");

    let development = std::fs::read_to_string(development_ir).expect("read development IR");
    let release = std::fs::read_to_string(release_ir).expect("read release IR");
    let development_definitions = development
        .lines()
        .filter(|line| line.starts_with("define "))
        .collect::<Vec<_>>();
    assert!(
        development.contains("define internal i32 @loom.fn.0.optimize_folded"),
        "{development_definitions:#?}"
    );
    assert!(!development.contains("define internal i32 @loom.fn.1.optimize_unreachable"));
    assert!(development.contains("llvm.sadd.with.overflow.i64"));
    assert!(!release.contains("define internal i32 @loom.fn.0.optimize_folded"));
    assert!(!release.contains("define internal i32 @loom.fn.1.optimize_unreachable"));
    assert!(!release.contains("llvm.sadd.with.overflow.i64"));
}

fn unit_program() -> Program {
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
    program
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
fn task_outcomes_match_and_expose_fault_details_natively() {
    let source = r#"module outcomes

async fn completed() Int {
    7
}

async fn faulted() Int {
    assert false
    0
}

pub async fn main() Unit {
    let success, failure = Task.settled(completed(), faulted()).await
    match success {
        Completed(value) => {
            assert value == 7
            Unit
        }
        Faulted(_) => {
            assert false
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
    match failure {
        Completed(_) => {
            assert false
            Unit
        }
        Faulted(fault) => {
            let code = fault.code()
            let message = fault.message()
            assert code == "TaskFault"
            assert message == "task execution failed"
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
    Unit
}
"#;
    let project = tempfile::tempdir().expect("create outcome project");
    std::fs::write(project.path().join("main.loom"), source).expect("write outcome source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load outcome project")
        .snapshot()
        .expect("analyze outcome project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower outcome MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit native outcome executable");
    let output = Command::new(executable)
        .output()
        .expect("run native outcomes");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "AssertionFault\nUnit\n"
    );
}

#[test]
fn duration_file_and_socket_tasks_run_natively() {
    use std::io::{Read, Write};

    let project = tempfile::tempdir().expect("create I/O project");
    let file = project.path().join("round-trip.txt");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept test client");
        let mut request = [0_u8; 4];
        socket.read_exact(&mut request).expect("read request");
        assert_eq!(&request, b"ping");
        socket.write_all(b"pong").expect("write response");
    });
    let source = format!(
        r#"module standard_io

import standard.time.milliseconds
import standard.file.open_read
import standard.file.create
import standard.net.connect

pub async fn main() Unit {{
    let delay = milliseconds(1)
    let observed = delay.as_milliseconds()
    assert observed == 1
    Task.sleep(delay).await
    {{
        scoped output = create("{}").await
        output.write_text("hello from loom").await
        Unit
    }}
    {{
        scoped input = open_read("{}").await
        let content = input.read_text().await
        assert content == "hello from loom"
        Unit
    }}
    {{
        scoped socket = connect("127.0.0.1", {}).await
        socket.write_text("ping").await
        let response = socket.read_text().await
        assert response == "pong"
        Unit
    }}
    Unit
}}
"#,
        file.display(),
        file.display(),
        port,
    );
    std::fs::write(project.path().join("main.loom"), source).expect("write I/O source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load I/O project")
        .snapshot()
        .expect("analyze I/O project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower I/O MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit native I/O executable");
    let output = Command::new(executable)
        .output()
        .expect("run native I/O program");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
    server.join().expect("join test server");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "hello from loom");
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
