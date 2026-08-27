#![allow(clippy::default_trait_access)]

use std::process::{Command, Output};

use loom_codegen_ir::{
    CheckedArtifact, LoweringOutcome, SourceArtifactRequest, TargetLayout, dump_program,
    lower_typed_artifact,
};
use loom_codegen_llvm::{
    DebugSource, EmitOptions, NativeObjectOptions, NativeRouteKind, NativeRoutePolicy,
    OptimizationProfile, emit_lcir_native_object, prepare_native_object,
};
use loom_core::runtime_fault::{INTEGER_OVERFLOW_FAULT_CODE, INTEGER_OVERFLOW_FAULT_MESSAGE};
use loom_driver::AnalysisHost;
use loom_interpreter::{ExecutionFailure, Interpreter, TestStatus, Value};
use loom_mir::{
    CheckedProgram, Constant as MirConstant, ContractExprKind, ExprKind, Function, Pattern,
    StatementKind, UnaryOp,
};
use loom_runtime_abi::{FAULT_FORMAT_ENV, FAULT_FORMAT_JSON, FAULT_JSON_PREFIX};

mod support;
use support::{emit_native, link_native_object};

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
    lower_source_artifact_with_layout(program, request, host_layout())
}

fn lower_source_artifact_with_layout(
    program: &CheckedProgram,
    request: &SourceArtifactRequest,
    layout: TargetLayout,
) -> CheckedArtifact {
    match lower_typed_artifact(program, request, layout).expect("classify typed LCIR") {
        LoweringOutcome::Complete(artifact) => artifact,
        LoweringOutcome::Unsupported(report) => {
            panic!("source fixture unexpectedly unsupported: {report:?}")
        }
    }
}

const PROJECTED_PLACE_SOURCE: &str =
    include_str!("../../../fixtures/lcir-projected-places/main.loom");

fn emit_and_run_lcir(artifact: &CheckedArtifact, stem: &str) -> NativeRun {
    emit_and_run_lcir_with_options(artifact, stem, NativeObjectOptions::default())
}

fn emit_and_run_lcir_machine_fault(artifact: &CheckedArtifact, stem: &str) -> NativeRun {
    emit_and_run_lcir_with_options_and_fault_format(
        artifact,
        stem,
        NativeObjectOptions::default(),
        true,
    )
}

fn emit_and_run_lcir_with_options(
    artifact: &CheckedArtifact,
    stem: &str,
    options: NativeObjectOptions,
) -> NativeRun {
    emit_and_run_lcir_with_options_and_fault_format(artifact, stem, options, false)
}

fn emit_and_run_lcir_with_options_and_fault_format(
    artifact: &CheckedArtifact,
    stem: &str,
    mut options: NativeObjectOptions,
    machine_faults: bool,
) -> NativeRun {
    let directory = tempfile::tempdir().expect("create LCIR output directory");
    let object = directory.path().join(format!("{stem}.o"));
    let ir = directory.path().join(format!("{stem}.ll"));
    let executable = directory.path().join(stem);
    options.emit_ir = Some(ir.clone());
    emit_lcir_native_object(artifact, &object, &options).expect("emit source-lowered LCIR object");
    link_native_object(&object, &executable).expect("link source-lowered LCIR executable");
    let mut command = Command::new(executable);
    if machine_faults {
        command.env(FAULT_FORMAT_ENV, FAULT_FORMAT_JSON);
    }
    let output = command
        .output()
        .expect("run source-lowered LCIR executable");
    NativeRun {
        ir: std::fs::read_to_string(ir).expect("read source-lowered LLVM IR"),
        output,
    }
}

fn emit_and_run_legacy(program: &CheckedProgram, entry: &str, stem: &str) -> Output {
    emit_and_run_legacy_with_fault_format(program, entry, stem, false)
}

fn emit_and_run_legacy_machine_fault(program: &CheckedProgram, entry: &str, stem: &str) -> Output {
    emit_and_run_legacy_with_fault_format(program, entry, stem, true)
}

fn emit_and_run_legacy_machine_fault_with_ir(
    program: &CheckedProgram,
    entry: &str,
    stem: &str,
) -> NativeRun {
    let directory = tempfile::tempdir().expect("create legacy output directory");
    let executable = directory.path().join(stem);
    let ir_path = directory.path().join(format!("{stem}.ll"));
    let mut options = EmitOptions::run(entry);
    options.emit_ir = Some(ir_path.clone());
    emit_native(program, &executable, &options).expect("emit legacy comparison executable");
    let output = Command::new(executable)
        .env(FAULT_FORMAT_ENV, FAULT_FORMAT_JSON)
        .output()
        .expect("run legacy comparison executable");
    NativeRun {
        ir: std::fs::read_to_string(ir_path).expect("read legacy LLVM IR"),
        output,
    }
}

fn emit_and_run_legacy_with_fault_format(
    program: &CheckedProgram,
    entry: &str,
    stem: &str,
    machine_faults: bool,
) -> Output {
    let directory = tempfile::tempdir().expect("create legacy output directory");
    let executable = directory.path().join(stem);
    emit_native(program, &executable, &EmitOptions::run(entry))
        .expect("emit legacy comparison executable");
    let mut command = Command::new(executable);
    if machine_faults {
        command.env(FAULT_FORMAT_ENV, FAULT_FORMAT_JSON);
    }
    command.output().expect("run legacy comparison executable")
}

fn emit_and_run_legacy_tests(program: &CheckedProgram, stem: &str) -> Output {
    let directory = tempfile::tempdir().expect("create legacy test output directory");
    let executable = directory.path().join(stem);
    emit_native(program, &executable, &EmitOptions::tests())
        .expect("emit legacy comparison test executable");
    Command::new(executable)
        .output()
        .expect("run legacy comparison test executable")
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

record Gauge {
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

impl Gauge {
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
    var gauge = Gauge { value = 4, enabled = false }
    gauge.reset(5)
    gauge.add(6)
    discard gauge.value
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
    let direct_inouts = ir
        .lines()
        .filter(|line| line.contains("name: \"LoomInOut<t1;writebacks=[t"))
        .collect::<Vec<_>>();
    assert_eq!(direct_inouts.len(), 2, "{direct_inouts:#?}\n{ir}");
    assert!(
        direct_inouts.iter().all(|line| {
            line.contains("size: 128, align: 64")
                && line.contains("DIFlagArtificial")
                && line.contains(
                    "identifier: \"loom.compiler.LoomReturn.inout.result.t1.writebacks.1.t",
                )
        }),
        "{direct_inouts:#?}\n{ir}"
    );
    assert_ne!(direct_inouts[0], direct_inouts[1], "{direct_inouts:#?}");
    let fallible_inouts = ir
        .lines()
        .filter(|line| line.contains("name: \"LoomFallibleInOut<t1;writebacks=[t"))
        .collect::<Vec<_>>();
    assert_eq!(fallible_inouts.len(), 2, "{fallible_inouts:#?}\n{ir}");
    assert!(
        fallible_inouts.iter().all(|line| {
            line.contains("size: 192, align: 64")
                && line.contains("DIFlagArtificial")
                && line.contains(
                    "identifier: \"loom.compiler.LoomReturn.fallible.result.t1.writebacks.1.t",
                )
        }),
        "{fallible_inouts:#?}\n{ir}"
    );
    assert_ne!(
        fallible_inouts[0], fallible_inouts[1],
        "{fallible_inouts:#?}"
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
    assert_eq!(writebacks.len(), 4, "{writebacks:#?}\n{ir}");
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

fn machine_fault(output: &Output) -> serde_json::Value {
    let stderr = String::from_utf8(output.stderr.clone()).expect("machine fault is UTF-8");
    let faults = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(FAULT_JSON_PREFIX))
        .map(|json| serde_json::from_str(json).expect("machine fault is valid JSON"))
        .collect::<Vec<_>>();
    assert_eq!(faults.len(), 1, "expected one machine fault: {output:?}");
    faults.into_iter().next().expect("one machine fault")
}

fn integer_overflow_fault(span: &impl serde::Serialize) -> serde_json::Value {
    serde_json::json!({
        "channel": "runtime",
        "fault": {
            "code": INTEGER_OVERFLOW_FAULT_CODE,
            "message": INTEGER_OVERFLOW_FAULT_MESSAGE,
            "span": span,
        },
    })
}

fn source_function<'program>(
    program: &'program CheckedProgram,
    suffix: &str,
) -> &'program Function {
    program
        .functions
        .iter()
        .find(|function| function.name.ends_with(suffix))
        .unwrap_or_else(|| panic!("source function ending in `{suffix}`"))
}

fn checked_float_pattern_fixture() -> CheckedProgram {
    let source = r"module lcir_float_patterns

fn classify(value Float) Int {
    match value {
        0.0 => 10
        1.0 => 20
        42.0 => 21
        _ => 30
    }
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
    requireEqual(classify(0.0), 10)
    requireEqual(classify(-0.0), 10)
    requireEqual(classify(1.0), 20)
    requireEqual(classify(42.0), 30)
    requireEqual(classify(0.0 / 0.0), 30)
    Unit
}
";
    let mut program = compile_source(source).into_program();
    let classify = program
        .functions
        .iter_mut()
        .find(|function| function.name.ends_with(".classify"))
        .expect("manual classify MIR");
    let ExprKind::Match { arms, .. } = &mut classify
        .body
        .tail
        .as_deref_mut()
        .expect("classify tail")
        .kind
    else {
        panic!("classify tail must remain a MIR match")
    };
    for (replacement, index) in [(-0.0, 0_usize), (f64::from_bits(0x7ff8_0000_0000_0042), 2)] {
        let Pattern::Constant(MirConstant::Float(value)) =
            &mut arms.get_mut(index).expect("manual float pattern").pattern
        else {
            panic!("edited pattern must be a float constant")
        };
        *value = replacement;
    }
    CheckedProgram::new(program).expect("manually edited IEEE-pattern MIR must validate")
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

fn assert_no_indirect_calls(ir: &str) {
    for line in ir.lines() {
        let Some(call) = line.find("call ") else {
            continue;
        };
        let callee_prefix = line[call + "call ".len()..]
            .split_once('(')
            .map_or(line, |(prefix, _)| prefix);
        assert!(
            callee_prefix.contains('@'),
            "indirect LLVM call in typed LCIR:\n{line}\n\n{ir}"
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

const LIVE_SUM_CARRIER_SOURCE: &str = r"module lcir_live_sum_carrier

enum Packet {
    Empty
    Wide(Int)
    Bytes(Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool)
}

enum Problem { WrongCarrier }

test fn carriesAcrossLoop() Result[Unit, Problem] {
    var packet = Packet.Empty
    for index in 0..1000 {
        packet = match packet {
            Empty => Packet.Wide(index)
            Wide(_) => Packet.Bytes(false, false, false, false, false, false, false, false, true)
            Bytes(_, _, _, _, _, _, _, _, _) => Packet.Empty
        }
        Unit
    }
    match packet {
        Wide(value) => if value == 999 { Ok(Unit) } else { Err(Problem.WrongCarrier) }
        _ => Err(Problem.WrongCarrier)
    }
}
";

#[test]
fn float_patterns_use_ieee_ordered_equality_in_all_three_backends() {
    let program = checked_float_pattern_fixture();
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = loom_codegen_ir::dump_program(artifact.program());
    assert_eq!(
        dump.matches("float.compare.ordered_equal").count(),
        2,
        "+0 must match a -0 pattern, while a NaN pattern is impossible:\n{dump}"
    );

    let lcir = emit_and_run_lcir(&artifact, "float-patterns");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-float-patterns");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, legacy.stdout);
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
fn pure_immortal_text_operations_need_no_active_runtime_gc_or_executor() {
    let program = compile_source(
        r#"module lcir_text_pure

fn inspect(value Text) Bool {
    value.length() == 6 && value.contains("界") && value == "hello界" && value != "other"
}

pub fn main() Unit {
    discard inspect("hello界")
    Unit
}
"#,
    );
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let native = emit_and_run_lcir(&artifact, "source-pure-immortal-text");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert!(
        native
            .ir
            .contains("declare i32 @loom_runtime_text_contains(ptr, i64, ptr, i64)"),
        "{}",
        native.ir
    );
    assert!(native.ir.contains("@loom_layout_text_v1 = external global"));
    assert!(
        !native.ir.contains("loom_runtime_create_v1"),
        "{}",
        native.ir
    );
    assert!(
        !native.ir.contains("loom_runtime_activate_v1"),
        "{}",
        native.ir
    );
    assert_no_legacy_surface(&native.ir);
}

#[test]
fn immortal_text_uses_one_pointer_and_allocation_free_runtime_abi_on_all_targets() {
    let source = include_str!("../../../fixtures/lcir-text/main.loom");
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    for expected in [
        "immortal_text_ptr",
        "types=[Text] witnesses=[]",
        "text.literal \"hello界\"",
        "text.length",
        "text.contains",
        "text.compare.equal",
        "text.compare.not_equal",
    ] {
        assert!(dump.contains(expected), "missing `{expected}`:\n{dump}");
    }

    let native = emit_and_run_lcir(&artifact, "source-immortal-text");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-immortal-text");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert_eq!(native.output.stdout, legacy.stdout);
    assert_eq!(native.output.stderr, legacy.stderr);
    assert!(
        native
            .ir
            .contains("declare i32 @loom_runtime_text_contains(ptr, i64, ptr, i64)"),
        "{}",
        native.ir
    );
    assert!(native.ir.contains("@loom_layout_text_v1 = external global"));
    assert!(native.ir.contains("text.compare.same_length"));
    assert!(native.ir.contains("define internal ptr @loom.lcir.fn"));
    assert_no_legacy_surface(&native.ir);

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create cross-target Text directory");
        let object = directory.path().join("text.o");
        let ir_path = directory.path().join("text.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit Text object for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read cross-target Text IR");
        assert!(
            ir.contains(&format!("target triple = \"{target}\"")),
            "{ir}"
        );
        assert!(
            ir.contains("declare i32 @loom_runtime_text_contains(ptr, i64, ptr, i64)"),
            "{ir}"
        );
        assert!(ir.contains("define internal ptr @loom.lcir.fn"), "{ir}");
        assert_no_legacy_surface(&ir);
    }
}

#[test]
fn generic_instances_use_direct_host_and_msvc_target_abis() {
    let source = include_str!("../../../fixtures/lcir-generics/main.loom");
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    for expected in [
        "source=f0 types=[Bool] witnesses=[]",
        "source=f0 types=[Float] witnesses=[]",
        "source=f0 types=[Int] witnesses=[]",
        "source=f1 types=[Int] witnesses=[Concrete#0]",
    ] {
        assert!(dump.contains(expected), "missing `{expected}`:\n{dump}");
    }

    let native = emit_and_run_lcir_with_options(
        &artifact,
        "source-generics",
        NativeObjectOptions {
            optimization: OptimizationProfile::Development,
            ..NativeObjectOptions::default()
        },
    );
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    for signature in [
        "define internal i1 @loom.lcir.fn.0(i1",
        "define internal double @loom.lcir.fn.1(double",
        "define internal i64 @loom.lcir.fn.2(i64",
        "define internal i64 @loom.lcir.fn.3(i64",
    ] {
        assert!(
            native.ir.contains(signature),
            "missing `{signature}`:\n{}",
            native.ir
        );
    }
    assert_pure_surface(&native.ir);

    let legacy = emit_and_run_legacy(&program, "main", "source-generics-legacy");
    assert_eq!(legacy.status.success(), native.output.status.success());
    assert_eq!(legacy.stdout, native.output.stdout);
    assert_eq!(legacy.stderr, native.output.stderr);

    let directory = tempfile::tempdir().expect("create MSVC generic output directory");
    let object = directory.path().join("generic.obj");
    let ir_path = directory.path().join("generic-msvc.ll");
    emit_lcir_native_object(
        &artifact,
        &object,
        &NativeObjectOptions {
            optimization: OptimizationProfile::Development,
            target_triple: Some("x86_64-pc-windows-msvc".to_owned()),
            emit_ir: Some(ir_path.clone()),
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit direct generic MSVC object");
    assert!(object.is_file());
    let msvc_ir = std::fs::read_to_string(ir_path).expect("read generic MSVC IR");
    assert!(
        msvc_ir.contains("target triple = \"x86_64-pc-windows-msvc\""),
        "{msvc_ir}"
    );
    assert!(
        msvc_ir.contains("define internal i64 @loom.lcir.fn.2(i64"),
        "{msvc_ir}"
    );
    assert_pure_surface(&msvc_ir);
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
fn integer_overflow_json_matches_at_each_direct_operation_span() {
    let source = r"module lcir_integer_overflow_diagnostics

fn negate(value Int) Int { -value }
fn add(left Int, right Int) Int { left + right }
fn subtract(left Int, right Int) Int { left - right }
fn multiply(left Int, right Int) Int { left * right }

pub fn negateMain() Unit {
    discard negate(-9223372036854775808)
    Unit
}

pub fn addMain() Unit {
    discard add(9223372036854775807, 1)
    Unit
}

pub fn subtractMain() Unit {
    discard subtract(-9223372036854775808, 1)
    Unit
}

pub fn multiplyMain() Unit {
    discard multiply(9223372036854775807, 2)
    Unit
}
";
    let program = compile_source(source);

    for (entry, operation_name) in [
        ("negateMain", "negate"),
        ("addMain", "add"),
        ("subtractMain", "subtract"),
        ("multiplyMain", "multiply"),
    ] {
        let operation = source_function(&program, operation_name);
        let expression = operation.body.tail.as_deref().expect("operation tail");
        assert!(
            matches!(
                expression.kind,
                ExprKind::Unary(UnaryOp::Negate, _)
                    | ExprKind::Binary(
                        loom_mir::BinaryOp::Add
                            | loom_mir::BinaryOp::Subtract
                            | loom_mir::BinaryOp::Multiply,
                        _,
                        _,
                    )
            ),
            "{operation_name}: {expression:?}"
        );
        assert_ne!(
            expression.span, operation.span,
            "the operation fixture must distinguish expression and function spans"
        );
        let expected = integer_overflow_fault(&expression.span);
        let interpreted = serde_json::to_value(
            interpret_run(&program, entry).expect_err("interpreter integer overflow"),
        )
        .expect("serialize interpreter failure");
        assert_eq!(interpreted, expected, "interpreter {entry}");

        let artifact = lower_source_artifact(
            &program,
            &SourceArtifactRequest::Run {
                entry: entry.into(),
            },
        );
        let lcir =
            emit_and_run_lcir_machine_fault(&artifact, &format!("lcir-integer-overflow-{entry}"));
        let legacy = emit_and_run_legacy_machine_fault(
            &program,
            entry,
            &format!("legacy-integer-overflow-{entry}"),
        );
        assert!(!lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert!(!legacy.status.success(), "{entry}: {legacy:?}");
        assert_eq!(machine_fault(&lcir.output), expected, "LCIR {entry}");
        assert_eq!(machine_fault(&legacy), expected, "legacy {entry}");
        assert_fallible_surface(&lcir.ir);
    }
}

#[test]
fn provable_integer_arithmetic_remains_fault_free() {
    let source = r"module lcir_provable_integer_arithmetic

pub fn main() Unit {
    let value = (20 + 22) * 1 - 0
    assert value == 42
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
    let lcir = emit_and_run_lcir_machine_fault(&artifact, "lcir-provable-integer-arithmetic");
    let legacy = emit_and_run_legacy_machine_fault_with_ir(
        &program,
        "main",
        "legacy-provable-integer-arithmetic",
    );

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.output.status.success(), "{:?}", legacy.output);
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(legacy.output.stdout, lcir.output.stdout);
    assert!(!diagnostic_text(&lcir.output).contains(FAULT_JSON_PREFIX));
    assert!(!diagnostic_text(&legacy.output).contains(FAULT_JSON_PREFIX));
    assert!(
        !legacy.ir.contains("with.overflow"),
        "provable legacy arithmetic retained a runtime overflow check:\n{}",
        legacy.ir
    );
}

#[test]
fn contract_int_negation_overflow_matches_interpreter_and_legacy() {
    let source = r"module contract_int_negation

fn guarded(value Int) Unit
    requires -value >= 0
{
    Unit
}

fn returnMinimum() Int
    ensures -result >= 0
{
    -9223372036854775808
}

pub fn requiresMain() Unit {
    guarded(-9223372036854775808)
}

pub fn ensuresMain() Unit {
    discard returnMinimum()
    Unit
}

pub fn assertMain() Unit {
    let minimum = -9223372036854775808
    assert -minimum >= 0
    Unit
}
";
    let program = compile_source(source);

    let guarded = source_function(&program, "guarded");
    let requires_negation = match &guarded.call_plan.requires[0].expression.kind {
        ContractExprKind::Binary(_, left, _) => left.as_ref(),
        other => panic!("unexpected requires expression: {other:?}"),
    };
    let return_minimum = source_function(&program, "returnMinimum");
    let ensures_negation = match &return_minimum.call_plan.ensures[0].expression.kind {
        ContractExprKind::Binary(_, left, _) => left.as_ref(),
        other => panic!("unexpected ensures expression: {other:?}"),
    };
    let assert_main = source_function(&program, "assertMain");
    let assert_condition = assert_main
        .body
        .statements
        .iter()
        .find_map(|statement| match &statement.kind {
            StatementKind::Assert { condition } => Some(condition),
            _ => None,
        })
        .expect("assert condition");
    let assertion_negation = match &assert_condition.kind {
        ExprKind::Binary(_, left, _) => left.as_ref(),
        other => panic!("unexpected assertion expression: {other:?}"),
    };

    for expression in [requires_negation, ensures_negation] {
        assert!(
            matches!(expression.kind, ContractExprKind::Unary(UnaryOp::Negate, _)),
            "{expression:?}"
        );
    }
    assert!(
        matches!(assertion_negation.kind, ExprKind::Unary(UnaryOp::Negate, _)),
        "{assertion_negation:?}"
    );

    for (entry, operation_span, function_span) in [
        ("requiresMain", requires_negation.span, guarded.span),
        ("ensuresMain", ensures_negation.span, return_minimum.span),
        ("assertMain", assertion_negation.span, assert_main.span),
    ] {
        assert_ne!(
            operation_span, function_span,
            "the contract fixture must distinguish expression and function spans"
        );
        let expected = integer_overflow_fault(&operation_span);
        let failure = interpret_run(&program, entry).expect_err("interpreter overflow");
        assert_eq!(
            serde_json::to_value(failure).expect("serialize interpreter overflow"),
            expected,
            "interpreter {entry}"
        );

        let legacy = emit_and_run_legacy_machine_fault(
            &program,
            entry,
            &format!("legacy-contract-int-negation-{entry}"),
        );
        assert!(!legacy.status.success(), "{entry}: {legacy:?}");
        assert_eq!(machine_fault(&legacy), expected, "legacy {entry}");
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
fn projected_places_preserve_sibling_updates_and_loop_product_phis() {
    let program = compile_source(PROJECTED_PLACE_SOURCE);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare projected-place source");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.matches("product.extract").count() >= 8, "{dump}");
    assert!(dump.matches("product.insert").count() >= 4, "{dump}");
    assert!(
        dump.lines()
            .any(|line| line.trim_start().starts_with("b1(") && line.contains(": t7")),
        "the loop must carry a typed product block parameter:\n{dump}"
    );

    let lcir = emit_and_run_lcir(&artifact, "source-projected-places");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-projected-places");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    let lowered_functions = lcir.ir.split("define i32 @main").next().unwrap_or(&lcir.ir);
    for forbidden in [
        "alloca",
        "memcpy",
        "loom.Value",
        "loom_gc_",
        "loom_executor_",
    ] {
        assert!(
            !lowered_functions.contains(forbidden),
            "unexpected `{forbidden}` in projected-place LCIR:\n{lowered_functions}"
        );
    }
    assert!(
        lcir.ir.contains("insertvalue { { { i64 }, { i64 } }, i1 }"),
        "nested Holder reconstruction must use its exact physical product:\n{}",
        lcir.ir
    );
    assert_fallible_surface(&lcir.ir);
}

#[test]
fn nested_receiver_aliases_preserve_root_to_leaf_projection_order() {
    let source = r"module lcir_nested_receiver_aliases

record Counter { value Int }
record Pair { left Counter, right Counter }
record Holder { guard Int, pair Pair }

impl Counter {
    method add(mut self, amount Int) Unit {
        self.value = self.value + amount
    }
}

impl Pair {
    method bumpLeft(mut self) Unit {
        self.left.add(5)
    }
}

pub fn main() Unit {
    var holder = Holder {
        guard = 7,
        pair = Pair {
            left = Counter { value = 11 },
            right = Counter { value = 29 },
        },
    }
    holder.pair.bumpLeft()
    let guard = holder.guard
    let left = holder.pair.left.value
    let right = holder.pair.right.value
    if guard == 7 && left == 16 && right == 29 {
        Unit
    } else {
        discard 1 / 0
        Unit
    }
}
";
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare nested projected receivers");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let lcir = emit_and_run_lcir(&artifact, "source-nested-receiver-aliases");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-nested-receiver-aliases");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, legacy.stdout);
}

#[test]
fn projected_place_products_emit_exact_i686_and_msvc_objects() {
    let program = compile_source(PROJECTED_PLACE_SOURCE);
    let request = SourceArtifactRequest::Run {
        entry: "crossTarget".into(),
    };
    let cases = [
        (
            "i686-unknown-linux-gnu",
            TargetLayout::new(32).expect("i686 layout"),
            "projected-i686.o",
            &b"\x7fELF"[..],
        ),
        (
            "x86_64-pc-windows-msvc",
            TargetLayout::new(64).expect("MSVC layout"),
            "projected-msvc.obj",
            &b"\x64\x86"[..],
        ),
    ];
    for (triple, layout, filename, magic) in cases {
        let artifact = lower_source_artifact_with_layout(&program, &request, layout);
        let directory = tempfile::tempdir().expect("create cross-target directory");
        let object = directory.path().join(filename);
        let ir_path = directory.path().join(format!("{filename}.ll"));
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(triple.to_owned()),
                emit_ir: Some(ir_path.clone()),
                optimization: OptimizationProfile::Development,
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit projected-place object for {triple}: {error}"));
        let bytes = std::fs::read(&object).expect("read cross-target object");
        assert!(bytes.starts_with(magic), "wrong object format for {triple}");
        let ir = std::fs::read_to_string(ir_path).expect("read cross-target IR");
        assert!(
            ir.contains(&format!("target triple = \"{triple}\"")),
            "{ir}"
        );
        assert!(
            ir.contains("insertvalue { { { i64 }, { i64 } }, i1 }"),
            "{ir}"
        );
        assert_pure_surface(&ir);
    }
}

#[test]
fn source_tuples_cross_direct_abi_and_destructure_across_three_backends() {
    let source = r"module lcir_source_tuples

record Packet { pair (Int, Bool) }

fn rearrange(input (Packet, Float)) (Bool, Packet) {
    let packet, ignored = input
    discard ignored
    let number, enabled = packet.pair
    (enabled, Packet { pair = (number, enabled) })
}

fn requireEqual(actual Int, expected Int) Unit {
    if actual == expected { Unit } else {
        discard 1 / 0
        Unit
    }
}

pub fn main() Unit {
    let enabled, packet = rearrange((Packet { pair = (40, true) }, 1.5))
    let number, copied = packet.pair
    if enabled && copied {
        requireEqual(number, 40)
    } else {
        discard 1 / 0
        Unit
    }
}
";
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare automatic tuple route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let lcir = emit_and_run_lcir_with_options(
        &artifact,
        "source-tuples",
        NativeObjectOptions {
            debug_sources: vec![DebugSource::new(0, "main.loom", source)],
            ..NativeObjectOptions::default()
        },
    );
    let legacy = emit_and_run_legacy(&program, "main", "legacy-tuples");

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    assert!(
        lcir.ir.contains(
            "define internal { i1, { { i64, i1 } } } @loom.lcir.fn.0({ { { i64, i1 } }, double } %arg0)"
        ),
        "tuple arguments and results must stay in the direct physical ABI:\n{}",
        lcir.ir
    );
    assert!(lcir.ir.contains("insertvalue"), "{}", lcir.ir);
    assert!(lcir.ir.contains("extractvalue"), "{}", lcir.ir);
    assert!(lcir.ir.contains("name: \"LoomProduct<t"), "{}", lcir.ir);
    let lowered_functions = lcir.ir.split("define i32 @main").next().unwrap_or(&lcir.ir);
    for forbidden in [
        "alloca",
        "memcpy",
        "loom.Value",
        "loom_gc_",
        "loom_executor_",
    ] {
        assert!(
            !lowered_functions.contains(forbidden),
            "unexpected `{forbidden}` in tuple LCIR:\n{lowered_functions}"
        );
    }
    assert_fallible_surface(&lcir.ir);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end fixture keeps transparent, invariant-product, direct-sum, three-backend, release, and debug evidence together"
)]
fn proven_refinements_and_invariant_records_are_zero_cost_typed_lcir_values() {
    let source = r"module lcir_source_refined

type Money = Float where self >= 0.0

record Range {
    low Money
    high Money
    invariant self.low <= self.high
}

enum Holding {
    Empty
    Cash(Money)
    Window(Range)
}

fn established() (Money, Range) {
    let money = Money(10.0)
    let range = Range { low = Money(1.0), high = Money(2.0) }
    (money, range)
}

fn widen(value Money) Float {
    value
}

fn value(input Holding) Float {
    match input {
        Empty => 0.0
        Cash(money) => money
        Window(range) => {
            discard range
            2.0
        }
    }
}

pub fn main() Unit {
    let money, range = established()
    let cash = value(Holding.Cash(money))
    let window = value(Holding.Window(range))
    if widen(money) == 10.0 && cash == 10.0 && window == 2.0 {
        Unit
    } else {
        discard 1 / 0
        Unit
    }
}
";
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare automatic refined route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("refine.proven"), "{dump}");
    assert!(dump.contains("unrefine"), "{dump}");
    assert!(dump.contains("invariant_record.proven"), "{dump}");
    assert!(dump.contains("transparent(t4)"), "{dump}");
    assert!(dump.contains("invariant_product"), "{dump}");
    assert!(dump.contains("sum.construct"), "{dump}");
    assert!(dump.contains("sum.switch"), "{dump}");

    let lcir = emit_and_run_lcir_with_options(
        &artifact,
        "source-refined",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );
    let legacy = emit_and_run_legacy(&program, "main", "legacy-refined");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    let lowered_functions = lcir.ir.split("define i32 @main").next().unwrap_or(&lcir.ir);
    for forbidden in [
        "alloca",
        "memcpy",
        "loom.Value",
        "loom_gc_",
        "loom_executor_",
        "loom_context_",
        "indirect",
    ] {
        assert!(
            !lowered_functions.contains(forbidden),
            "unexpected `{forbidden}` in proven refined LCIR:\n{lowered_functions}"
        );
    }
    let debug = emit_and_run_lcir_with_options(
        &artifact,
        "source-refined-debug",
        NativeObjectOptions::default().with_debug_sources(vec![DebugSource::new(
            0,
            "main.loom",
            source,
        )]),
    );
    assert!(debug.output.status.success(), "{:?}", debug.output);
    assert_eq!(debug.output.stdout, legacy.stdout);
    assert!(debug.ir.contains("switch i8"), "{}", debug.ir);
    assert!(
        debug.ir.contains("!DIBasicType(name: \"Float\", size: 64"),
        "transparent scalar debug metadata must use its physical base type:\n{}",
        debug.ir
    );
    assert!(
        !debug.ir.contains("name: \"Money\"")
            && !debug.ir.contains("name: \"Range\"")
            && !debug.ir.contains("LoomTransparent"),
        "the current physical debug boundary must not pretend to preserve nominal wrappers:\n{}",
        debug.ir
    );
}

#[test]
fn runtime_refinement_checks_still_select_atomic_fallback() {
    let source = r"module lcir_source_dynamic_refined

type Money = Float where self >= 0.0

fn checked(raw Float) Result[Money, ConstraintError] {
    Money(raw)
}

pub fn main() Unit {
    discard checked(-1.0)
    Unit
}
";
    let program = compile_source(source);
    let outcome = lower_typed_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        host_layout(),
    )
    .expect("classify runtime refinement");
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("runtime refinement must not enter the proven-only LCIR slice")
    };
    assert!(
        report
            .items()
            .iter()
            .any(|item| { item.feature() == loom_codegen_ir::UnsupportedFeature::RefinedValue }),
        "{report:?}"
    );
}

#[test]
fn release_tuple_ir_needs_no_storage_runtime_or_executor_surface() {
    let source = r"module lcir_release_tuples

record Packet { pair (Int, Bool) }

fn roundTrip(input (Packet, Float)) (Bool, Packet) {
    let packet, ignored = input
    discard ignored
    let number, enabled = packet.pair
    (enabled, Packet { pair = (number, enabled) })
}

pub fn main() Unit {
    let enabled, packet = roundTrip((Packet { pair = (40, true) }, 1.5))
    discard enabled
    let number, copied = packet.pair
    discard number
    discard copied
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
    let release = emit_and_run_lcir_with_options(
        &artifact,
        "release-tuples",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );

    assert!(release.output.status.success(), "{:?}", release.output);
    assert_eq!(release.output.stdout, b"Unit\n");
    for forbidden in [
        "alloca",
        "memcpy",
        "loom.Value",
        "loom_runtime_",
        "loom_gc_",
        "loom_executor_",
    ] {
        assert!(
            !release.ir.contains(forbidden),
            "unexpected `{forbidden}` in release tuple IR:\n{}",
            release.ir
        );
    }
    assert_pure_surface(&release.ir);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end fixture keeps tagless, tag-only, aligned tagged, nested aggregate, dead managed, and three-backend evidence together"
)]
fn closed_sums_cross_exact_abi_and_match_across_three_backends() {
    let source = r"module lcir_source_sums

enum Single { Wrapped(Int) }

enum Flag { Off, On }

enum Odd {
    Empty
    Wide(Int)
    Bytes(Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool)
}

enum Dead { Managed(Text) }

fn unreachableManaged(value Text) Dead { Dead.Managed(value) }

record Envelope { value Odd }

enum Container {
    Boxed(Envelope)
    Paired((Int, Bool))
}

fn unwrap(input Single) Int {
    match input { Wrapped(value) => value }
}

fn flag(input Flag) Int {
    match input {
        Off => 0
        On => 1
    }
}

fn odd(input Odd) Int {
    match input {
        Empty => 0
        Wide(0) => 700
        Wide(value) => value
        Bytes(a, b, c, d, e, f, g, h, i) => {
            discard a
            discard b
            discard c
            discard d
            discard e
            discard f
            discard g
            discard h
            if i { 9 } else { 8 }
        }
    }
}

fn container(input Container) Int {
    match input {
        Boxed(envelope) => odd(envelope.value)
        Paired(pair) => {
            let value, enabled = pair
            if enabled { value } else { 0 }
        }
    }
}

fn requireEqual(actual Int, expected Int) Unit {
    if actual == expected { Unit } else {
        discard 1 / 0
        Unit
    }
}

pub fn main() Unit {
    requireEqual(unwrap(Single.Wrapped(41)), 41)
    requireEqual(flag(Flag.Off), 0)
    requireEqual(flag(Flag.On), 1)
    requireEqual(odd(Odd.Empty), 0)
    requireEqual(odd(Odd.Wide(0)), 700)
    requireEqual(odd(Odd.Wide(73)), 73)
    requireEqual(odd(Odd.Bytes(false, false, false, false, false, false, false, false, true)), 9)
    requireEqual(container(Container.Boxed(Envelope { value = Odd.Wide(81) })), 81)
    requireEqual(container(Container.Paired((12, true))), 12)
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
    .expect("prepare automatic sum route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let lcir = emit_and_run_lcir_with_options(
        &artifact,
        "source-sums",
        NativeObjectOptions {
            debug_sources: vec![DebugSource::new(0, "main.loom", source)],
            ..NativeObjectOptions::default()
        },
    );
    let legacy = emit_and_run_legacy(&program, "main", "legacy-sums");

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    assert!(lcir.ir.contains("switch i8"), "{}", lcir.ir);
    assert!(lcir.ir.contains("name: \"LoomSum<t"), "{}", lcir.ir);
    assert!(
        lcir.ir.lines().any(|line| {
            line.contains("name: \"LoomSum<t")
                && line.contains("size: 192")
                && line.contains("align: 64")
        }),
        "the 9-byte/align-8 carrier must round to an exact 24-byte tagged ABI:\n{}",
        lcir.ir
    );
    assert_no_indirect_calls(&lcir.ir);
    assert_no_legacy_surface(&lcir.ir);
}

#[test]
fn release_sum_ir_eliminates_carrier_scratch_and_runtime_surfaces() {
    let source = r"module lcir_release_sums

enum Odd {
    Empty
    Wide(Int)
    Bytes(Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool)
}

fn score(input Odd) Int {
    match input {
        Empty => 0
        Wide(value) => value
        Bytes(a, b, c, d, e, f, g, h, i) => {
            discard a
            discard b
            discard c
            discard d
            discard e
            discard f
            discard g
            discard h
            if i { 9 } else { 8 }
        }
    }
}

pub fn main() Unit {
    discard score(Odd.Empty)
    discard score(Odd.Wide(73))
    discard score(Odd.Bytes(false, false, false, false, false, false, false, false, true))
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
    let release = emit_and_run_lcir_with_options(
        &artifact,
        "release-sums",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );

    assert!(release.output.status.success(), "{:?}", release.output);
    assert_eq!(release.output.stdout, b"Unit\n");
    for forbidden in [
        "alloca",
        "memcpy",
        "loom.Value",
        "loom_runtime_",
        "loom_gc_",
        "loom_executor_",
    ] {
        assert!(
            !release.ir.contains(forbidden),
            "unexpected `{forbidden}` in release sum IR:\n{}",
            release.ir
        );
    }
    assert_no_indirect_calls(&release.ir);
    assert_pure_surface(&release.ir);
}

#[test]
fn release_keeps_a_live_sum_carrier_in_register_ssa() {
    let program = compile_source(LIVE_SUM_CARRIER_SOURCE);
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1);
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let release = emit_and_run_lcir_with_options(
        &artifact,
        "release-live-sum",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );

    assert!(release.output.status.success(), "{:?}", release.output);
    assert!(release.ir.contains("switch i8"), "{}", release.ir);
    assert!(release.ir.contains(" = phi { i8, {"), "{}", release.ir);
    assert!(release.ir.contains(" phi i64 "), "{}", release.ir);
    for forbidden in [
        "alloca",
        "memcpy",
        "loom.Value",
        "loom_gc_",
        "loom_executor_",
    ] {
        assert!(
            !release.ir.contains(forbidden),
            "unexpected `{forbidden}` in live release carrier IR:\n{}",
            release.ir
        );
    }
    assert_no_indirect_calls(&release.ir);
}

#[test]
fn closed_sum_carriers_emit_as_native_msvc_objects_without_fallback() {
    let program = compile_source(LIVE_SUM_CARRIER_SOURCE);
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let directory = tempfile::tempdir().expect("create MSVC sum output directory");
    let object = directory.path().join("sum.obj");
    let ir_path = directory.path().join("sum-msvc.ll");
    emit_lcir_native_object(
        &artifact,
        &object,
        &NativeObjectOptions {
            emit_ir: Some(ir_path.clone()),
            target_triple: Some("x86_64-pc-windows-msvc".to_owned()),
            optimization: OptimizationProfile::Release,
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit direct closed-sum MSVC object");
    assert!(object.is_file());
    let object_bytes = std::fs::read(&object).expect("read MSVC object");
    assert_eq!(
        object_bytes.get(..2),
        Some([0x64, 0x86].as_slice()),
        "x86_64 MSVC output must be a real AMD64 COFF object"
    );
    let ir = std::fs::read_to_string(ir_path).expect("read MSVC sum IR");
    for forbidden in [
        "alloca",
        "memcpy",
        "loom.Value",
        "loom_runtime_",
        "loom_gc_",
        "loom_executor_",
    ] {
        assert!(!ir.contains(forbidden), "unexpected `{forbidden}`:\n{ir}");
    }
    assert!(ir.contains("switch i8"), "{ir}");
    assert!(ir.contains(" = phi { i8, {"), "{ir}");
    assert!(ir.contains(" phi i64 "), "{ir}");
    assert_no_indirect_calls(&ir);
    assert_pure_surface(&ir);
}

#[test]
fn result_unit_test_outcomes_drive_native_and_legacy_harnesses() {
    let source = r"module lcir_result_tests

enum Problem { Failed(Int) }

test fn succeeds() Result[Unit, Problem] { Ok(Unit) }

test fn fails() Result[Unit, Problem] { Err(Problem.Failed(7)) }
";
    let program = compile_source(source);
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 2);
    assert_eq!(
        interpreted
            .iter()
            .find(|result| result.name == "lcir_result_tests.succeeds")
            .expect("success test")
            .status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    assert_eq!(
        interpreted
            .iter()
            .find(|result| result.name == "lcir_result_tests.fails")
            .expect("failure test")
            .status,
        TestStatus::Failed,
        "{interpreted:#?}"
    );

    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let lcir = emit_and_run_lcir(&artifact, "result-tests");
    let legacy = emit_and_run_legacy_tests(&program, "legacy-result-tests");
    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    assert!(!legacy.status.success(), "{legacy:?}");
    let stdout = String::from_utf8(lcir.output.stdout).expect("UTF-8 LCIR test output");
    assert!(
        stdout.contains("passed lcir_result_tests.succeeds"),
        "{stdout}"
    );
    assert!(
        stdout.contains("failed lcir_result_tests.fails"),
        "{stdout}"
    );
    assert!(lcir.ir.contains("test.result.succeeded"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("loom_runtime_"), "{}", lcir.ir);
    assert_no_indirect_calls(&lcir.ir);
    assert_no_legacy_surface(&lcir.ir);
}

#[test]
fn fallible_result_test_checks_runtime_status_before_the_sum_outcome() {
    let source = r"module lcir_fallible_result_tests

enum Problem { Failed }

test fn passes() Result[Unit, Problem] { Ok(Unit) }

test fn faults() Result[Unit, Problem] {
    discard 1 / 0
    Ok(Unit)
}
";
    let program = compile_source(source);
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 2);
    assert_eq!(
        interpreted
            .iter()
            .find(|result| result.name == "lcir_fallible_result_tests.passes")
            .expect("passing test")
            .status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    assert_eq!(
        interpreted
            .iter()
            .find(|result| result.name == "lcir_fallible_result_tests.faults")
            .expect("faulting test")
            .status,
        TestStatus::Failed,
        "{interpreted:#?}"
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let lcir = emit_and_run_lcir_with_options(
        &artifact,
        "fallible-result-tests",
        NativeObjectOptions {
            debug_sources: vec![DebugSource::new(0, "main.loom", source)],
            ..NativeObjectOptions::default()
        },
    );
    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    let stdout = String::from_utf8(lcir.output.stdout).expect("UTF-8 LCIR test output");
    assert!(
        stdout.contains("passed lcir_fallible_result_tests.passes"),
        "{stdout}"
    );
    assert!(
        stdout.contains("failed lcir_fallible_result_tests.faults"),
        "{stdout}"
    );
    assert!(lcir.ir.contains("test.outcome.succeeded"), "{}", lcir.ir);
    assert!(
        lcir.ir.contains("name: \"LoomFallible<LoomSum<t"),
        "{}",
        lcir.ir
    );
    assert_no_indirect_calls(&lcir.ir);
    assert_fallible_surface(&lcir.ir);
}

#[test]
fn nested_projected_fault_edges_reconstruct_each_mutable_receiver() {
    let source = r"module lcir_source_record_fault

record Counter { value Int }
record Pair { left Counter, right Counter }
record Holder { pair Pair, guard Int }

impl Counter {
    method mutateThenFail(mut self) Unit {
        self.value = 9
        discard 1 / 0
        Unit
    }
}

impl Holder {
    method cascade(mut self) Unit {
        self.pair.left.mutateThenFail()
        Unit
    }
}

pub fn main() Unit {
    var holder = Holder {
        pair = Pair {
            left = Counter { value = 1 },
            right = Counter { value = 2 },
        },
        guard = 7,
    }
    holder.cascade()
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
    let dump = dump_program(artifact.program());
    assert!(
        dump.matches("product.insert").count() >= 4,
        "normal and fault writebacks must reconstruct nested roots:\n{dump}"
    );
    assert!(dump.contains("resume_fault writebacks"), "{dump}");
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
    assert!(
        lcir.ir
            .contains("{ i32, {}, { { { i64 }, { i64 } }, i64 } }"),
        "{}",
        lcir.ir
    );
    assert!(
        lcir.ir
            .matches("insertvalue { { { i64 }, { i64 } }, i64 }")
            .count()
            >= 2,
        "{}",
        lcir.ir
    );
    assert_fallible_surface(&lcir.ir);
}
