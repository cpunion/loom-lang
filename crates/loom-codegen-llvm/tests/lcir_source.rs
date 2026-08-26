#![allow(clippy::default_trait_access)]

use std::process::{Command, Output};

use loom_codegen_ir::{
    CheckedArtifact, LoweringOutcome, SourceArtifactRequest, TargetLayout, lower_typed_artifact,
};
use loom_codegen_llvm::{
    DebugSource, EmitOptions, NativeObjectOptions, NativeRouteKind, NativeRoutePolicy,
    emit_lcir_native_object, emit_native, link_native_object, prepare_native_object,
};
use loom_driver::AnalysisHost;
use loom_interpreter::{ExecutionFailure, Interpreter, TestStatus, Value};
use loom_mir::CheckedProgram;

struct NativeRun {
    ir: String,
    output: Output,
}

fn compile_source(source: &str) -> CheckedProgram {
    let project = tempfile::tempdir().expect("create source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source fixture");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load source project")
        .snapshot()
        .expect("analyze source project");
    assert!(
        !snapshot.has_errors(),
        "source diagnostics: {:#?}",
        snapshot.diagnostics()
    );
    snapshot.executable().expect("lower checked MIR").clone()
}

fn host_layout() -> TargetLayout {
    TargetLayout::new(u16::try_from(usize::BITS).expect("host pointer width fits u16"))
        .expect("supported host target layout")
}

fn lower_source_artifact(
    program: &CheckedProgram,
    request: &SourceArtifactRequest,
) -> CheckedArtifact {
    match lower_typed_artifact(program, request, host_layout()).expect("classify typed LCIR") {
        LoweringOutcome::Complete(artifact) => artifact,
        LoweringOutcome::Unsupported(report) => {
            panic!("source fixture unexpectedly unsupported: {report:?}")
        }
    }
}

fn emit_and_run_lcir(artifact: &CheckedArtifact, stem: &str) -> NativeRun {
    let directory = tempfile::tempdir().expect("create LCIR output directory");
    let object = directory.path().join(format!("{stem}.o"));
    let ir = directory.path().join(format!("{stem}.ll"));
    let executable = directory.path().join(stem);
    let options = NativeObjectOptions {
        emit_ir: Some(ir.clone()),
        ..NativeObjectOptions::default()
    };
    emit_lcir_native_object(artifact, &object, &options).expect("emit source-lowered LCIR object");
    link_native_object(&object, &executable).expect("link source-lowered LCIR executable");
    let output = Command::new(executable)
        .output()
        .expect("run source-lowered LCIR executable");
    NativeRun {
        ir: std::fs::read_to_string(ir).expect("read source-lowered LLVM IR"),
        output,
    }
}

fn emit_and_run_legacy(program: &CheckedProgram, entry: &str, stem: &str) -> Output {
    let directory = tempfile::tempdir().expect("create legacy output directory");
    let executable = directory.path().join(stem);
    emit_native(program, &executable, &EmitOptions::run(entry))
        .expect("emit legacy comparison executable");
    Command::new(executable)
        .output()
        .expect("run legacy comparison executable")
}

#[test]
fn fallible_debug_metadata_describes_the_physical_abi_and_visible_parameters() {
    let source = include_str!("../../../fixtures/lcir-debug-fallible/main.loom");
    let program = compile_source(source);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let directory = tempfile::tempdir().expect("create debug output directory");
    let object = directory.path().join("fallible-debug.o");
    let ir_path = directory.path().join("fallible-debug.ll");
    let options = NativeObjectOptions {
        emit_ir: Some(ir_path.clone()),
        debug_sources: vec![DebugSource::new(0, "main.loom", source)],
        ..NativeObjectOptions::default()
    };
    emit_lcir_native_object(&artifact, &object, &options).expect("emit fallible debug object");
    let ir = std::fs::read_to_string(ir_path).expect("read fallible debug IR");

    assert!(
        ir.contains(
            "define internal { i32, i64 } @loom.lcir.fn.0(i64 %arg0, ptr %__loom_fault_context)"
        ),
        "{ir}"
    );
    assert!(ir.contains("name: \"LoomFallible<Int>\""), "{ir}");
    assert!(ir.contains("name: \"status\""), "{ir}");
    assert!(ir.contains("name: \"value\""), "{ir}");
    assert!(ir.contains("name: \"arg0\", arg: 1"), "{ir}");
    assert!(
        ir.contains("name: \"__loom_fault_context\", arg: 2"),
        "{ir}"
    );
    assert!(ir.contains("flags: DIFlagArtificial"), "{ir}");
    assert_eq!(
        ir.matches("#dbg_value(i64 %arg0").count(),
        2,
        "fallible and return-only parameter records must both survive:\n{ir}"
    );
    assert!(ir.contains("#dbg_value(ptr %__loom_fault_context"), "{ir}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn product_debug_metadata_matches_direct_and_fallible_inout_physical_returns() {
    let source = r"module lcir_product_debug

record Counter {
    value Int
    enabled Bool
}

impl Counter {
    method reset(mut self, value Int) Unit {
        self.value = value
        Unit
    }

    method add(mut self, value Int) Unit {
        self.value = self.value + value
        Unit
    }
}

fn forward(value Counter) Counter {
    value
}

pub fn main() Unit {
    var counter = Counter { value = 1, enabled = true }
    counter.reset(2)
    counter.add(3)
    let copied = forward(counter)
    discard copied.value
    Unit
}
";
    let program = compile_source(source);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let directory = tempfile::tempdir().expect("create product debug output directory");
    let object = directory.path().join("product-debug.o");
    let ir_path = directory.path().join("product-debug.ll");
    emit_lcir_native_object(
        &artifact,
        &object,
        &NativeObjectOptions {
            emit_ir: Some(ir_path.clone()),
            debug_sources: vec![DebugSource::new(0, "main.loom", source)],
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit product debug object");
    let ir = std::fs::read_to_string(ir_path).expect("read product debug IR");

    assert!(
        ir.contains("define internal { {}, { i64, i1 } } @loom.lcir.fn."),
        "infallible inout must return its functional writeback:\n{ir}"
    );
    assert!(
        ir.contains("define internal { i32, {}, { i64, i1 } } @loom.lcir.fn."),
        "fallible inout must return status, result, and writeback:\n{ir}"
    );
    let product = ir
        .lines()
        .find(|line| line.contains("name: \"LoomProduct<t"))
        .unwrap_or_else(|| panic!("missing compiler-private product debug type:\n{ir}"));
    assert!(
        product.contains("size: 128, align: 64") && product.contains("DIFlagArtificial"),
        "{product}\n{ir}"
    );
    let direct_inout = ir
        .lines()
        .find(|line| line.contains("name: \"LoomInOut<t1;1>\""))
        .unwrap_or_else(|| panic!("missing direct inout return debug type:\n{ir}"));
    assert!(
        direct_inout.contains("size: 128, align: 64") && direct_inout.contains("DIFlagArtificial"),
        "{direct_inout}\n{ir}"
    );
    let fallible_inout = ir
        .lines()
        .find(|line| line.contains("name: \"LoomFallibleInOut<t1;1>\""))
        .unwrap_or_else(|| panic!("missing fallible inout return debug type:\n{ir}"));
    assert!(
        fallible_inout.contains("size: 192, align: 64")
            && fallible_inout.contains("DIFlagArtificial"),
        "{fallible_inout}\n{ir}"
    );
    assert!(
        ir.lines().any(|line| {
            line.contains("name: \"field1\"")
                && line.contains("size: 1")
                && line.contains("offset: 64")
        }),
        "the product Bool field must use its target-data offset:\n{ir}"
    );
    let writebacks = ir
        .lines()
        .filter(|line| line.contains("name: \"writeback0\""))
        .collect::<Vec<_>>();
    assert_eq!(writebacks.len(), 2, "{writebacks:#?}\n{ir}");
    assert!(
        writebacks
            .iter()
            .all(|line| line.contains("DIFlagArtificial")),
        "{writebacks:#?}\n{ir}"
    );
    assert!(
        writebacks.iter().any(|line| line.contains("offset: 64")),
        "fallible writeback must follow the padded status/result prefix:\n{ir}"
    );
    assert!(
        ir.lines()
            .filter(|line| line.starts_with("define internal "))
            .all(|line| line.contains(" !dbg !")),
        "no product-bearing function may silently lose its subprogram type:\n{ir}"
    );
    assert!(!ir.contains("loom.Value"), "{ir}");
}

fn interpret_run(program: &CheckedProgram, entry: &str) -> Result<Value, ExecutionFailure> {
    let function_id = program.exports.get(entry).copied().expect("source export");
    let span = program.function(function_id).expect("source function").span;
    Interpreter::new(program).invoke(function_id, Vec::new(), span)
}

fn diagnostic_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_no_legacy_surface(ir: &str) {
    for forbidden in [
        "loom.Value",
        "ArgNode",
        "ValueNode",
        "loom.runtime.print",
        "loom_executor_",
        "loom_gc_",
        "witness",
        "landingpad",
        "personality ptr",
        "resume {",
    ] {
        assert!(
            !ir.contains(forbidden),
            "legacy/EH token `{forbidden}` in source-lowered IR:\n{ir}"
        );
    }
}

fn assert_pure_surface(ir: &str) {
    assert_no_legacy_surface(ir);
    assert!(!ir.contains("loom_runtime_"), "{ir}");
    assert!(!ir.contains("loom_context_raise_fault_v1"), "{ir}");
}

fn assert_fallible_surface(ir: &str) {
    assert_no_legacy_surface(ir);
    assert!(ir.contains("loom_runtime_create_v1"), "{ir}");
    assert!(ir.contains("loom_runtime_activate_v1"), "{ir}");
    assert!(ir.contains("loom_context_raise_fault_v1"), "{ir}");
    assert!(!ir.contains("loom_executor_"), "{ir}");
    assert!(!ir.contains("loom_gc_"), "{ir}");
}

#[test]
fn source_lowered_pure_scalars_run_without_runtime_or_legacy_values() {
    let source = r"module lcir_source_pure

fn choose(flag Bool, left Float, right Float) Bool {
    if flag { left < right } else { !flag }
}

pub fn main() Unit {
    discard choose(true, 1.0, 2.0)
    Unit
}
";
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let native = emit_and_run_lcir(&artifact, "source-pure");

    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert!(native.ir.contains("fcmp olt"), "{}", native.ir);
    assert_pure_surface(&native.ir);
}

#[test]
fn source_ranges_emit_proved_nsw_successors_without_fault_abi() {
    let source = r"module lcir_source_proved_ranges

fn highBit() Int {
    var seen = 0
    for index in 9223372036854775806..9223372036854775807 {
        seen = index
        Unit
    }
    seen
}

fn nested(outer Int, inner Int) Int {
    var seen = 0
    for first in 0..outer {
        for second in 0..inner {
            seen = second
            Unit
        }
        seen = first
        Unit
    }
    seen
}

pub fn main() Unit {
    discard highBit()
    discard nested(3, 4)
    Unit
}
";
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    assert!(
        artifact
            .functions()
            .iter()
            .all(|function| function.effects().is_empty()),
        "pure ranges must not acquire MAY_FAULT"
    );

    let native = emit_and_run_lcir(&artifact, "source-proved-ranges");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert_eq!(native.ir.matches("add nsw i64").count(), 3, "{}", native.ir);
    for forbidden in [
        "with.overflow",
        "invoke.status",
        "fault.status",
        "loom_runtime_",
        "loom_context_raise_fault_v1",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "unexpected fallible surface `{forbidden}`:\n{}",
            native.ir
        );
    }
    assert_pure_surface(&native.ir);
}

#[test]
fn recursive_and_iterative_source_computation_agree_across_backends() {
    let source = r"module lcir_source_fibonacci

fn recursive(value Int) Int {
    if value < 2 {
        value
    } else {
        recursive(value - 1) + recursive(value - 2)
    }
}

fn iterative(limit Int) Int {
    var previous = 0
    var current = 1
    for index in 0..limit {
        let next = previous + current
        previous = current
        current = next
        Unit
    }
    previous
}

fn highBit() Int {
    var seen = 0
    for index in 9223372036854775806..9223372036854775807 {
        seen = index
        Unit
    }
    seen
}

fn requireEqual(actual Int, expected Int) Unit {
    if actual == expected {
        Unit
    } else {
        discard 1 / 0
        Unit
    }
}

pub fn main() Unit {
    requireEqual(recursive(10), 55)
    requireEqual(iterative(10), 55)
    requireEqual(highBit(), 9223372036854775806)
    Unit
}
";
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let lcir = emit_and_run_lcir(&artifact, "source-fibonacci");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-fibonacci");

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    assert_fallible_surface(&lcir.ir);
}

#[test]
fn source_short_circuit_never_executes_its_faulting_rhs() {
    let source = r"module lcir_source_short_circuit

fn trap() Bool {
    discard 1 / 0
    true
}

pub fn main() Unit {
    discard false && trap()
    discard true || trap()
    Unit
}
";
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let lcir = emit_and_run_lcir(&artifact, "source-short-circuit");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-short-circuit");

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    assert_fallible_surface(&lcir.ir);
}

#[test]
fn source_integer_faults_match_interpreter_and_legacy_diagnostics() {
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
    ];

    for (name, statement, expected) in cases {
        let source = format!(
            "module lcir_source_{name}\n\npub fn main() Unit {{\n    {statement}\n    Unit\n}}\n"
        );
        let program = compile_source(&source);
        let failure = interpret_run(&program, "main").expect_err("interpreter fault");
        assert!(
            matches!(failure, ExecutionFailure::Runtime { ref fault } if fault.code == expected),
            "{name}: {failure:?}"
        );
        let artifact = lower_source_artifact(
            &program,
            &SourceArtifactRequest::Run {
                entry: "main".into(),
            },
        );
        let lcir = emit_and_run_lcir(&artifact, &format!("source-{name}"));
        let legacy = emit_and_run_legacy(&program, "main", &format!("legacy-{name}"));

        assert!(!lcir.output.status.success(), "{name}: {:?}", lcir.output);
        assert!(!legacy.status.success(), "{name}: {legacy:?}");
        assert!(
            diagnostic_text(&lcir.output).contains(expected),
            "{name}: {:?}",
            lcir.output
        );
        assert!(
            diagnostic_text(&legacy).contains(expected),
            "{name}: {legacy:?}"
        );
        assert_fallible_surface(&lcir.ir);
    }
}

#[test]
fn source_test_roots_preserve_declaration_order_in_one_pure_artifact() {
    let source = r"module lcir_source_tests

test fn zeta() { Unit }
test fn alpha() { Unit }
test fn middle() { Unit }
";
    let program = compile_source(source);
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 3);
    assert!(
        interpreted
            .iter()
            .all(|result| result.status == TestStatus::Passed),
        "{interpreted:#?}"
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let root_names = artifact
        .test_roots()
        .expect("test roots")
        .iter()
        .map(|root| artifact.function(*root).expect("test function").name())
        .collect::<Vec<_>>();
    assert_eq!(
        root_names,
        [
            "lcir_source_tests.zeta",
            "lcir_source_tests.alpha",
            "lcir_source_tests.middle",
        ]
    );
    let native = emit_and_run_lcir(&artifact, "source-tests");

    assert!(native.output.status.success(), "{:?}", native.output);
    let stdout = String::from_utf8(native.output.stdout).expect("UTF-8 test output");
    let results = stdout
        .lines()
        .filter(|line| line.starts_with("passed ") || line.starts_with("failed "))
        .collect::<Vec<_>>();
    assert_eq!(
        results,
        [
            "passed lcir_source_tests.zeta",
            "passed lcir_source_tests.alpha",
            "passed lcir_source_tests.middle",
        ]
    );
    assert_pure_surface(&native.ir);
}

#[test]
fn source_pod_records_use_direct_ssa_products_and_functional_receiver_writeback() {
    let source = r"module lcir_source_records

record Counter {
    total Int
    calls Int
}

record Holder {
    counter Counter
    enabled Bool
}

impl Holder {
    method setTotal(mut self, value Int) Unit {
        self.counter.total = value
        Unit
    }
}

impl Counter {
    method add(mut self, value Int) Unit {
        self.total = self.total + value
        self.calls = self.calls + 1
        Unit
    }
}

fn periodicValue(index Int) Int {
    index - (index / 8) * 8
}

fn recordMethod(size Int) Counter {
    var counter = Counter { total = 0, calls = 0 }
    for index in 0..size {
        counter.add(periodicValue(index))
        Unit
    }
    counter
}

fn nestedUpdate() Holder {
    var holder = Holder {
        counter = Counter { total = 1, calls = 2 },
        enabled = true,
    }
    holder.setTotal(11)
    holder
}

fn requireEqual(actual Int, expected Int) Unit {
    if actual == expected {
        Unit
    } else {
        discard 1 / 0
        Unit
    }
}

pub fn main() Unit {
    var original = Counter { total = 3, calls = 4 }
    var copied = original
    copied.add(5)
    requireEqual(original.total, 3)
    requireEqual(original.calls, 4)
    requireEqual(copied.total, 8)
    requireEqual(copied.calls, 5)
    let looped = recordMethod(10)
    requireEqual(looped.total, 29)
    requireEqual(looped.calls, 10)
    let holder = nestedUpdate()
    requireEqual(holder.counter.total, 11)
    requireEqual(holder.counter.calls, 2)
    Unit
}
";
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare automatic record route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let lcir = emit_and_run_lcir(&artifact, "source-records");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-records");

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    let lowered_functions = lcir.ir.split("define i32 @main").next().unwrap_or(&lcir.ir);
    for forbidden in ["alloca", "loom.Value", "loom_gc_", "loom_executor_"] {
        assert!(
            !lowered_functions.contains(forbidden),
            "unexpected `{forbidden}`:\n{lowered_functions}"
        );
    }
    assert!(lcir.ir.contains("insertvalue { i64, i64 }"), "{}", lcir.ir);
    assert!(lcir.ir.contains("extractvalue { i64, i64 }"), "{}", lcir.ir);
    assert_fallible_surface(&lcir.ir);
}

#[test]
fn source_record_fault_edges_return_the_latest_receiver_writeback() {
    let source = r"module lcir_source_record_fault

record Counter { value Int }

impl Counter {
    method mutateThenFail(mut self) Unit {
        self.value = 9
        discard 1 / 0
        Unit
    }
}

pub fn main() Unit {
    var counter = Counter { value = 1 }
    counter.mutateThenFail()
    Unit
}
";
    let program = compile_source(source);
    let failure = interpret_run(&program, "main").expect_err("interpreter fault");
    assert!(
        matches!(failure, ExecutionFailure::Runtime { ref fault } if fault.code == "IntegerDivisionByZero"),
        "{failure:?}"
    );
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let lcir = emit_and_run_lcir(&artifact, "source-record-fault");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-record-fault");

    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    assert!(!legacy.status.success(), "{legacy:?}");
    assert!(
        diagnostic_text(&lcir.output).contains("IntegerDivisionByZero"),
        "{:?}",
        lcir.output
    );
    assert!(
        diagnostic_text(&legacy).contains("IntegerDivisionByZero"),
        "{legacy:?}"
    );
    assert!(lcir.ir.contains("{ i32, {}, { i64 } }"), "{}", lcir.ir);
    assert!(lcir.ir.contains("insertvalue { i64 }"), "{}", lcir.ir);
    assert_fallible_surface(&lcir.ir);
}
