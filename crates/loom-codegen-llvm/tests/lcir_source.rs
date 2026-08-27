#![allow(clippy::default_trait_access)]

use std::collections::BTreeMap;
use std::process::{Command, Output};

use loom_codegen_ir::{
    CheckedArtifact, Effects, LoweringOutcome, SourceArtifactRequest, TargetLayout, dump_program,
    lower_typed_artifact,
};
use loom_codegen_llvm::{
    DebugSource, EmitOptions, NativeObjectOptions, NativeRouteKind, NativeRoutePolicy,
    OptimizationProfile, emit_lcir_native_object, prepare_native_object,
};
use loom_core::{
    Span,
    runtime_fault::{INTEGER_OVERFLOW_FAULT_CODE, INTEGER_OVERFLOW_FAULT_MESSAGE},
};
use loom_driver::AnalysisHost;
use loom_interpreter::{ExecutionFailure, Interpreter, TestStatus, Value};
use loom_mir::{
    Block, CallPlan, CheckedProgram, Constant as MirConstant, ConstructionMode, ContractExprKind,
    Expr, ExprKind, FieldDef, Function, FunctionId, LocalDecl, LocalId, Pattern, PreludeIds,
    Program, ScopedDisposal, Statement, StatementKind, Type, TypeDef, TypeDefKind, TypeId, UnaryOp,
};
use loom_runtime_abi::{
    FAULT_FORMAT_ENV, FAULT_FORMAT_JSON, FAULT_JSON_PREFIX, FORMAT_FLOAT_TYPED_SYMBOL,
    PARSE_FLOAT_SYMBOL, PARSE_INT_SYMBOL,
};

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

fn emitted_lcir_function<'ir>(ir: &'ir str, artifact: &CheckedArtifact, suffix: &str) -> &'ir str {
    let function = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with(suffix))
        .unwrap_or_else(|| panic!("LCIR function ending in `{suffix}`"));
    let symbol = format!("@loom.lcir.fn.{}(", function.id().raw());
    let symbol_at = ir
        .find(&symbol)
        .unwrap_or_else(|| panic!("emitted LCIR function `{symbol}`"));
    let start = ir[..symbol_at]
        .rfind("\ndefine ")
        .map_or(0, |offset| offset + 1);
    let end = ir[symbol_at..]
        .find("\n}")
        .map_or(ir.len(), |offset| symbol_at + offset + 2);
    &ir[start..end]
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

fn checked_builtin_file_cleanup_fixture() -> CheckedProgram {
    let span = Span::default();
    let file_id = TypeId(9);
    let file = Type::Nominal(file_id, Vec::new());
    let mut types = (0_u32..9)
        .map(|id| TypeDef {
            id: TypeId(id),
            name: format!("Placeholder{id}"),
            span,
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: Vec::new(),
                invariant: None,
            },
        })
        .collect::<Vec<_>>();
    types.push(TypeDef {
        id: file_id,
        name: "File".into(),
        span,
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "raw".into(),
                ty: Type::Int,
                span,
            }],
            invariant: None,
        },
    });
    let mut program = Program {
        exports: BTreeMap::from([("main".into(), FunctionId(0))]),
        types,
        functions: vec![Function {
            id: FunctionId(0),
            name: "manual.main".into(),
            span,
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: vec![LocalDecl {
                id: LocalId(0),
                name: "file".into(),
                ty: file.clone(),
                mutable: true,
                span,
            }],
            return_ty: Type::Unit,
            receiver: None,
            body: Block {
                statements: vec![Statement {
                    kind: StatementKind::Scoped {
                        local: LocalId(0),
                        value: Expr::new(
                            ExprKind::Record {
                                ty: file_id,
                                type_arguments: Vec::new(),
                                fields: vec![Expr::new(
                                    ExprKind::Constant(MirConstant::Int(-1)),
                                    Type::Int,
                                    span,
                                )],
                                construction: ConstructionMode::Plain,
                            },
                            file,
                            span,
                        ),
                        disposal: ScopedDisposal::FileClose,
                    },
                    span,
                }],
                tail: Some(Box::new(Expr::new(
                    ExprKind::Constant(MirConstant::Unit),
                    Type::Unit,
                    span,
                ))),
                span,
            },
            call_plan: CallPlan::default(),
        }],
        prelude: PreludeIds {
            file: Some(file_id),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    program
        .renumber_expr_ids()
        .expect("number resource fixture");
    program
        .into_checked()
        .expect("canonical built-in cleanup fixture must validate")
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
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate covers every scalar builtin status, managed Text input, target object, and legacy-surface exclusion"
)]
fn scalar_builtins_match_interpreter_and_legacy_without_universal_values() {
    let source = r#"module lcir_scalar_builtins

import standard.float.is_finite
import standard.float.parse_float
import standard.int.parse_int
import standard.time.milliseconds

fn join(left Text, right Text) Text { left.concat(right) }

fn parsedIntEquals(input Text, expected Int) Bool {
    match parse_int(input) {
        Ok(value) => value == expected
        Err(_) => false
    }
}

fn intInvalid(input Text) Bool {
    match parse_int(input) {
        Err(ParseIntError.InvalidSyntax) => true
        _ => false
    }
}

fn intOutOfRange(input Text) Bool {
    match parse_int(input) {
        Err(ParseIntError.OutOfRange) => true
        _ => false
    }
}

fn parsedFloatEquals(input Text, expected Float) Bool {
    match parse_float(input) {
        Ok(value) => value == expected
        Err(_) => false
    }
}

fn floatInvalid(input Text) Bool {
    match parse_float(input) {
        Err(standard.float.ParseFloatError.InvalidSyntax) => true
        _ => false
    }
}

fn floatOutOfRange(input Text) Bool {
    match parse_float(input) {
        Err(standard.float.ParseFloatError.OutOfRange) => true
        _ => false
    }
}

pub fn main() Unit {
    let managed = join("92", "23372036854775807")
    let maxInt = parsedIntEquals(managed, 9223372036854775807)
    let minInt = parsedIntEquals("-9223372036854775808", -9223372036854775807 - 1)
    let plusInt = parsedIntEquals("+17", 17)
    let invalidInt = intInvalid("17x")
    let positiveIntOverflow = intOutOfRange("9223372036854775808")
    let negativeIntOverflow = intOutOfRange("-9223372036854775809")
    assert maxInt
    assert minInt
    assert plusInt
    assert invalidInt
    assert positiveIntOverflow
    assert negativeIntOverflow

    let finiteFloat = parsedFloatEquals("1.25e2", 125.0)
    let positiveInfinity = parsedFloatEquals("Infinity", 1.0 / 0.0)
    let negativeInfinity = parsedFloatEquals("-Infinity", -1.0 / 0.0)
    let parsedNaN = match parse_float("NaN") {
        Ok(value) => !is_finite(value)
        Err(_) => false
    }
    let parsedNegativeZero = match parse_float("-0.0") {
        Ok(value) => 1.0 / value == -1.0 / 0.0
        Err(_) => false
    }
    let invalidFloat = floatInvalid("1")
    let floatOverflow = floatOutOfRange("1e999")
    let finiteZero = is_finite(0.0)
    let finiteNegativeZero = is_finite(-0.0)
    let finiteNaN = is_finite(0.0 / 0.0)
    let finiteInfinity = is_finite(1.0 / 0.0)
    assert finiteFloat
    assert positiveInfinity
    assert negativeInfinity
    assert parsedNaN
    assert parsedNegativeZero
    assert invalidFloat
    assert floatOverflow
    assert finiteZero
    assert finiteNegativeZero
    assert !finiteNaN
    assert !finiteInfinity

    let delay = milliseconds(42)
    let observed = delay.as_milliseconds()
    assert observed == 42
    Unit
}
"#;
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    for required in [
        "parse.int",
        "parse.float",
        "float.compare.ordered_greater_equal",
        "float.compare.ordered_less_equal",
        "runtime InvalidDuration",
        "product.construct",
        "product.extract",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    let native = emit_and_run_lcir(&artifact, "source-scalar-builtins");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-scalar-builtins");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(native.output.stdout, legacy.stdout);
    assert_eq!(native.output.stderr, legacy.stderr);
    for required in [
        PARSE_INT_SYMBOL,
        PARSE_FLOAT_SYMBOL,
        "parse.int.status.valid",
        "parse.float.status.valid",
        "parse.int.status.failed",
        "parse.float.status.failed",
        "call void @llvm.trap()",
    ] {
        assert!(
            native.ir.contains(required),
            "scalar builtin IR omitted `{required}`:\n{}",
            native.ir
        );
    }
    for parse in ["parse.int", "parse.float"] {
        let failed = format!("{parse}.status.failed:");
        let start = native
            .ir
            .rfind(&failed)
            .unwrap_or_else(|| panic!("missing unexpected-status block `{failed}`"));
        let end = native.ir.len().min(start + 512);
        assert!(
            native.ir[start..end].contains("call void @llvm.trap()"),
            "{parse} must trap an ABI-forged status outside 0/1/2:\n{}",
            &native.ir[start..end]
        );
    }
    let parse_integer = emitted_lcir_function(&native.ir, &artifact, "parsedIntEquals");
    assert!(!parse_integer.contains("loom_gc_typed_root_push_v1"));
    assert!(!parse_integer.contains("loom_gc_typed_root_pop_v1"));
    assert_no_legacy_surface(&native.ir);

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create scalar builtin target directory");
        let object = directory.path().join("scalar-builtins.o");
        let ir_path = directory.path().join("scalar-builtins.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit scalar builtin object for {target}: {error}"));
        assert!(
            object.is_file(),
            "missing scalar builtin object for {target}"
        );
        let ir = std::fs::read_to_string(ir_path).expect("read scalar builtin target IR");
        assert!(ir.contains(PARSE_INT_SYMBOL), "{ir}");
        assert!(ir.contains(PARSE_FLOAT_SYMBOL), "{ir}");
        assert_no_legacy_surface(&ir);
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate covers canonical formatting, moving roots, runtime status integrity, and portable objects"
)]
fn typed_float_formatting_matches_all_backends_and_preserves_moving_text() {
    let pressure = "x".repeat(40 * 1024);
    let source = format!(
        r#"module lcir_typed_float_format

import standard.float.format_float

fn join(left Text, right Text) Text {{ left.concat(right) }}

fn render(value Float) Text {{ format_float(value) }}

pub fn main() Unit {{
    let kept = join("K", "eep")
    let pressure = "{pressure}".concat("{pressure}")
    discard pressure.length()
    let finite = render(1.25)
    let integral = render(1e20)
    let small = render(1e-7)
    let negativeZero = render(-0.0)
    let positiveInfinity = render(1.0 / 0.0)
    let negativeInfinity = render(-1.0 / 0.0)
    let notANumber = render(0.0 / 0.0)
    assert kept == "Keep"
    assert finite == "1.25"
    assert integral == "100000000000000000000.0"
    assert small == "0.0000001"
    assert negativeZero == "-0.0"
    assert positiveInfinity == "Infinity"
    assert negativeInfinity == "-Infinity"
    assert notANumber == "NaN"
    Unit
}}
"#
    );
    let program = compile_source(&source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let render = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("render"))
        .expect("typed formatter function");
    assert!(render.effects().contains(Effects::MAY_COLLECT));
    assert!(render.effects().contains(Effects::NEEDS_RUNTIME));
    assert!(!render.effects().contains(Effects::MAY_FAULT));
    assert!(!render.effects().contains(Effects::NEEDS_EXECUTOR));
    assert!(!render.effects().contains(Effects::MAY_SUSPEND));
    let dump = dump_program(artifact.program());
    assert!(dump.contains("format.float"), "{dump}");

    let native = emit_and_run_lcir(&artifact, "source-typed-float-format");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-float-format");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(native.output.stdout, legacy.stdout);
    assert_eq!(native.output.stderr, legacy.stderr);
    for required in [
        FORMAT_FLOAT_TYPED_SYMBOL,
        "format.float.failed",
        "call void @llvm.trap()",
        "managed.root.reload",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            native.ir.contains(required),
            "typed format IR omitted `{required}`:\n{}",
            native.ir
        );
    }
    let failed = native
        .ir
        .rfind("format.float.failed:")
        .expect("unexpected format status block");
    let failed_end = native.ir.len().min(failed + 512);
    assert!(
        native.ir[failed..failed_end].contains("call void @llvm.trap()"),
        "unexpected typed formatter status must trap:\n{}",
        &native.ir[failed..failed_end]
    );
    for forbidden in [
        "@loom_runtime_format_float(",
        "%loom.Value",
        "loom_gc_root_push_v1",
        "loom_executor_",
        "landingpad",
        "personality ptr",
    ] {
        assert!(!native.ir.contains(forbidden), "{}", native.ir);
    }

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create float format target directory");
        let object = directory.path().join("float-format.o");
        let ir_path = directory.path().join("float-format.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit typed float format object for {target}: {error}"));
        assert!(
            object.is_file(),
            "missing typed float format object for {target}"
        );
        let ir = std::fs::read_to_string(ir_path).expect("read typed float format target IR");
        assert!(ir.contains(FORMAT_FLOAT_TYPED_SYMBOL), "{ir}");
        assert!(ir.contains("loom_gc_typed_root_push_v1"), "{ir}");
        assert!(!ir.contains("@loom_runtime_format_float("), "{ir}");
        assert!(!ir.contains("%loom.Value"), "{ir}");
        assert!(!ir.contains("loom_executor_"), "{ir}");
    }
}

#[test]
fn negative_duration_fault_matches_interpreter_and_legacy() {
    let source = r"module lcir_negative_duration

import standard.time.milliseconds

pub fn main() Unit {
    discard milliseconds(-1)
    Unit
}
";
    let program = compile_source(source);
    let interpreted = serde_json::to_value(
        interpret_run(&program, "main").expect_err("negative Duration must fault"),
    )
    .expect("serialize Duration fault");
    assert_eq!(interpreted["fault"]["code"], "InvalidDuration");
    assert_eq!(
        interpreted["fault"]["message"],
        "Duration milliseconds cannot be negative"
    );
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("assert"), "{dump}");
    assert!(dump.contains("InvalidDuration"), "{dump}");
    let lcir = emit_and_run_lcir_machine_fault(&artifact, "lcir-negative-duration");
    let legacy = emit_and_run_legacy_machine_fault(&program, "main", "legacy-negative-duration");
    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    assert!(!legacy.status.success(), "{legacy:?}");
    let lcir_fault = machine_fault(&lcir.output);
    let legacy_fault = machine_fault(&legacy);
    assert_eq!(lcir_fault["fault"]["code"], "InvalidDuration");
    assert_eq!(
        lcir_fault["fault"]["message"],
        "Duration milliseconds cannot be negative"
    );
    assert_eq!(legacy_fault["fault"]["code"], interpreted["fault"]["code"]);
    assert_eq!(
        legacy_fault["fault"]["message"],
        interpreted["fault"]["message"]
    );
    assert_fallible_surface(&lcir.ir);
}

#[test]
fn invalid_duration_during_cleanup_cannot_replace_the_primary_fault() {
    let source = r"module lcir_duration_cleanup_fault

import standard.time.milliseconds

pub fn main() Unit {
    defer {
        discard milliseconds(-1)
    }
    discard 1 / 0
    Unit
}
";
    let program = compile_source(source);
    let interpreted = serde_json::to_value(
        interpret_run(&program, "main").expect_err("the body must originate the primary fault"),
    )
    .expect("serialize primary fault");
    assert_eq!(interpreted["fault"]["code"], "IntegerDivisionByZero");

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("assert"), "{dump}");
    assert!(dump.contains("InvalidDuration"), "{dump}");
    let lcir = emit_and_run_lcir_machine_fault(&artifact, "lcir-duration-cleanup-primary");
    let legacy =
        emit_and_run_legacy_machine_fault(&program, "main", "legacy-duration-cleanup-primary");
    let lcir_fault = machine_fault(&lcir.output);
    let legacy_fault = machine_fault(&legacy);
    assert_eq!(lcir_fault["code"], interpreted["fault"]["code"]);
    assert_eq!(legacy_fault["fault"]["code"], interpreted["fault"]["code"]);
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
    assert!(
        artifact
            .functions()
            .iter()
            .all(|function| function.effects() == Effects::NONE),
        "literal-only Text must remain effect-free:\n{}",
        dump_program(artifact.program())
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
#[expect(
    clippy::too_many_lines,
    reason = "one source fixture keeps interpreter/native semantics, forced relocation, IR shape, and cross-target objects in one differential gate"
)]
fn managed_text_concat_runs_tests_reloads_roots_and_emits_on_all_supported_targets() {
    let pressure = "x".repeat(40 * 1024);
    let source = format!(
        r#"module lcir_managed_text

enum Problem {{ WrongText }}

fn join(left Text, right Text) Text {{ left.concat(right) }}

pub fn main() Unit {{
    discard join("hello", "界").length()
    Unit
}}

test fn concatMovesAndAliases() Result[Unit, Problem] {{
    let kept = join("K", "eep")
    let pressure = "{pressure}".concat("{pressure}")
    discard pressure.length()
    let alias = kept.concat(kept)
    if alias == "KeepKeep" && kept == "Keep" {{
        Ok(Unit)
    }} else {{
        Err(Problem.WrongText)
    }}
}}
"#
    );
    let program = compile_source(&source);
    let run_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    assert!(run_artifact.functions().iter().all(|function| {
        function.effects().contains(Effects::MAY_COLLECT)
            && function.effects().contains(Effects::NEEDS_RUNTIME)
            && !function.effects().contains(Effects::MAY_FAULT)
            && !function.effects().contains(Effects::NEEDS_EXECUTOR)
            && !function.effects().contains(Effects::MAY_SUSPEND)
    }));
    let run = emit_and_run_lcir(&run_artifact, "source-managed-text-run");
    assert!(run.output.status.success(), "{:?}", run.output);
    assert_eq!(run.output.stdout, b"Unit\n");
    assert!(
        run.ir
            .contains("declare i32 @loom_runtime_text_concat_typed_v1(ptr, ptr, ptr)"),
        "{}",
        run.ir
    );
    assert!(run.ir.contains("loom_runtime_create_v1"), "{}", run.ir);
    assert!(run.ir.contains("loom_runtime_activate_v1"), "{}", run.ir);
    assert!(!run.ir.contains("loom_executor_"), "{}", run.ir);
    assert!(!run.ir.contains("%loom.Value"), "{}", run.ir);
    assert!(!run.ir.contains("loom_gc_root_push_v1"), "{}", run.ir);
    assert!(!run.ir.contains("loom_gc_typed_root_push_v1"), "{}", run.ir);
    assert!(!run.ir.contains("loom_gc_typed_root_pop_v1"), "{}", run.ir);

    let tests_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(tests_artifact.program());
    assert!(dump.contains("managed_ptr"), "{dump}");
    assert!(dump.contains("text.concat"), "{dump}");
    let interpreted = Interpreter::new(&program).run_tests();
    assert!(
        interpreted
            .iter()
            .all(|test| test.status == TestStatus::Passed),
        "{interpreted:?}"
    );
    let tests = emit_and_run_lcir(&tests_artifact, "source-managed-text-tests");
    assert!(tests.output.status.success(), "{:?}", tests.output);
    assert!(
        String::from_utf8_lossy(&tests.output.stdout).contains("concatMovesAndAliases"),
        "{:?}",
        tests.output
    );
    for required in [
        "loom_runtime_text_concat_typed_v1",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
        "managed.root.reload",
    ] {
        assert!(
            tests.ir.contains(required),
            "missing `{required}`:\n{}",
            tests.ir
        );
    }
    assert_eq!(
        tests
            .ir
            .matches("call i32 @loom_gc_typed_root_push_v1")
            .count(),
        tests
            .ir
            .matches("call i32 @loom_gc_typed_root_pop_v1")
            .count(),
        "typed root frames must balance on every generated exit:\n{}",
        tests.ir
    );
    assert!(!tests.ir.contains("loom_gc_root_push_v1"), "{}", tests.ir);
    assert!(!tests.ir.contains("loom_executor_"), "{}", tests.ir);
    assert!(!tests.ir.contains("%loom.Value"), "{}", tests.ir);

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create managed Text cross-target directory");
        let object = directory.path().join("managed-text.o");
        let ir_path = directory.path().join("managed-text.ll");
        emit_lcir_native_object(
            &tests_artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit managed Text object for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read managed Text cross-target IR");
        assert!(
            ir.contains(&format!("target triple = \"{target}\"")),
            "{ir}"
        );
        assert!(ir.contains("loom_runtime_text_concat_typed_v1"), "{ir}");
        assert!(ir.contains("loom_gc_typed_root_push_v1"), "{ir}");
        assert!(!ir.contains("loom_gc_root_push_v1"), "{ir}");
        assert!(!ir.contains("loom_executor_"), "{ir}");
        assert!(!ir.contains("%loom.Value"), "{ir}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate keeps Unicode scalar selection, missing indices, relocation, sum construction, and cross-target objects together"
)]
fn managed_text_get_returns_option_and_preserves_live_aliases_across_collection() {
    let pressure = "x".repeat(40 * 1024);
    let source = format!(
        r#"module lcir_managed_text_get

enum Problem {{ WrongText }}

fn join(left Text, right Text) Text {{ left.concat(right) }}

fn select(text Text, index Int) Option[Text] {{ text.get(index) }}

fn equals(input Option[Text], expected Text) Bool {{
    match input {{
        Some(value) => value == expected
        None => false
    }}
}}

fn missing(input Option[Text]) Bool {{
    match input {{
        Some(_) => false
        None => true
    }}
}}

test fn selectsUnicodeScalars() Result[Unit, Problem] {{
    let pressure = "{pressure}".concat("{pressure}")
    discard pressure.length()
    let kept = join("a界", "🙂z")
    let alias = kept
    let selected = select(kept, 1)
    let emoji = select(kept, 2)
    let negative = select(kept, -1)
    let pastEnd = select(kept, 4)
    if equals(selected, "界") && equals(emoji, "🙂") && missing(negative) && missing(pastEnd) && alias == "a界🙂z" {{
        Ok(Unit)
    }} else {{
        Err(Problem.WrongText)
    }}
}}

pub fn main() Unit {{
    discard "a界🙂z".get(1)
    Unit
}}
"#
    );
    let program = compile_source(&source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let interpreted = Interpreter::new(&program).run_tests();
    assert!(
        interpreted
            .iter()
            .all(|test| test.status == TestStatus::Passed),
        "{interpreted:?}"
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(artifact.program());
    assert!(
        dump.contains("text.get") && dump.contains("sum s"),
        "{dump}"
    );
    let selection_function = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("select"))
        .expect("selection function instance");
    assert!(selection_function.effects().contains(Effects::MAY_COLLECT));
    assert!(
        selection_function
            .effects()
            .contains(Effects::NEEDS_RUNTIME)
    );
    assert!(!selection_function.effects().contains(Effects::MAY_FAULT));
    assert!(
        !selection_function
            .effects()
            .contains(Effects::NEEDS_EXECUTOR)
    );
    assert!(
        artifact
            .functions()
            .iter()
            .all(|function| !function.effects().contains(Effects::NEEDS_EXECUTOR))
    );

    let native = emit_and_run_lcir(&artifact, "source-managed-text-get");
    let legacy = emit_and_run_legacy_tests(&program, "legacy-managed-text-get");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, legacy.stdout);
    assert_eq!(native.output.stderr, legacy.stderr);
    for required in [
        "loom_runtime_text_get_typed_v1",
        "text.get.status.valid",
        "text.get.status.failed",
        "text.get.option",
        "managed.root.reload",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            native.ir.contains(required),
            "Text.get IR omitted `{required}`:\n{}",
            native.ir
        );
    }
    for forbidden in [
        "%loom.Value",
        "loom_gc_root_push_v1",
        "loom_executor_",
        "landingpad",
        "personality ptr",
    ] {
        assert!(!native.ir.contains(forbidden), "{}", native.ir);
    }

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create Text.get target directory");
        let object = directory.path().join("text-get.o");
        let ir_path = directory.path().join("text-get.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit Text.get object for {target}: {error}"));
        assert!(object.is_file(), "missing Text.get object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read Text.get target IR");
        assert!(ir.contains("loom_runtime_text_get_typed_v1"), "{ir}");
        assert!(ir.contains("loom_gc_typed_root_push_v1"), "{ir}");
        assert!(!ir.contains("loom_gc_root_push_v1"), "{ir}");
        assert!(!ir.contains("%loom.Value"), "{ir}");
        assert!(!ir.contains("loom_executor_"), "{ir}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one adversarial gate keeps nested-product relocation, alias, phi, call, pointer-free, and cross-target evidence together"
)]
fn managed_product_leaves_relocate_exactly_across_collecting_calls() {
    let pressure = "x".repeat(40 * 1024);
    let source = format!(
        r#"module lcir_managed_products

enum Problem {{ WrongText }}

record Pair {{
    left Text
    right Text
}}

record Bundle {{
    pair Pair
    tail (Text, Int)
    enabled Bool
}}

impl Bundle {{
    method refresh(mut self) Unit {{
        let pressure = collectPressure()
        discard pressure.length()
        self.pair.left = self.pair.left.concat("")
        Unit
    }}
}}

fn join(left Text, right Text) Text {{ left.concat(right) }}

fn collectPressure() Text {{ "{pressure}".concat("{pressure}") }}

fn pointerFree(input (Int, Bool)) Int {{
    let number, enabled = input
    if enabled {{ number + 1 }} else {{ number }}
}}

fn retainParameter(input Bundle) Bundle {{
    let pressure = collectPressure()
    discard pressure.length()
    input
}}

fn retainDefinition(kept Text) Bundle {{
    let built = Bundle {{
        pair = Pair {{ left = kept, right = kept }},
        tail = (kept, 41),
        enabled = true,
    }}
    let pressure = collectPressure()
    discard pressure.length()
    built
}}

fn retain(input Bundle, takeInput Bool) Bundle {{
    let fallback = Bundle {{
        pair = Pair {{ left = "Fallback", right = "Fallback" }},
        tail = ("Fallback", 0),
        enabled = false,
    }}
    let selected = if takeInput {{ input }} else {{ fallback }}
    let pressure = collectPressure()
    discard pressure.length()
    selected
}}

fn retainInout(input Bundle) Bundle {{
    var retained = input
    retained.refresh()
    retained
}}

fn retainAcrossCleanup(input Bundle) Bundle {{
    defer {{
        let pressure = collectPressure()
        discard pressure.length()
        Unit
    }}
    input
}}

fn verify() Result[Unit, Problem] {{
    let kept = join("K", "eep")
    let input = retainDefinition(kept)
    let throughParameter = retainParameter(input)
    let retained = retainAcrossCleanup(retainInout(retain(throughParameter, true)))
    let tailText, number = retained.tail
    if retained.pair.left == "Keep" && retained.pair.right == "Keep" && tailText == "Keep" && retained.enabled && pointerFree((number, true)) == 42 {{
        Ok(Unit)
    }} else {{
        Err(Problem.WrongText)
    }}
}}

pub fn main() Unit {{
    discard verify()
    Unit
}}

test fn managedProducts() Result[Unit, Problem] {{ verify() }}
"#
    );
    let program = compile_source(&source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));

    let tests_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(tests_artifact.program());
    assert!(dump.contains("managed_ptr"), "{dump}");
    assert!(dump.contains("product"), "{dump}");
    let interpreted = Interpreter::new(&program).run_tests();
    assert!(
        interpreted
            .iter()
            .all(|test| test.status == TestStatus::Passed),
        "{interpreted:?}"
    );

    let native = emit_and_run_lcir(&tests_artifact, "source-managed-products");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(
        String::from_utf8_lossy(&native.output.stdout).contains("managedProducts"),
        "{:?}",
        native.output
    );
    let retain = emitted_lcir_function(&native.ir, &tests_artifact, "retain");
    for required in [
        "managed.root.v",
        ".p0.0",
        ".p0.1",
        ".p1.0",
        "managed.root.reload",
        "managed.root.rebuild",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(retain.contains(required), "missing `{required}`:\n{retain}");
    }
    assert_eq!(
        retain
            .matches("call i32 @loom_gc_typed_root_push_v1")
            .count(),
        retain
            .matches("call i32 @loom_gc_typed_root_pop_v1")
            .count(),
        "typed product root frame must balance:\n{retain}"
    );
    assert!(
        native.ir.contains("loom_runtime_text_concat_typed_v1"),
        "{}",
        native.ir
    );
    let retain_parameter = emitted_lcir_function(&native.ir, &tests_artifact, "retainParameter");
    for required in [
        ".p0.0",
        ".p0.1",
        ".p1.0",
        "managed.root.reload",
        "managed.root.rebuild",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            retain_parameter.contains(required),
            "entry product parameter omitted `{required}`:\n{retain_parameter}"
        );
    }
    let retain_definition = emitted_lcir_function(&native.ir, &tests_artifact, "retainDefinition");
    for required in [
        ".p0.0",
        ".p0.1",
        ".p1.0",
        "managed.root.reload",
        "managed.root.rebuild",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            retain_definition.contains(required),
            "defined product omitted `{required}`:\n{retain_definition}"
        );
    }
    let refresh = emitted_lcir_function(&native.ir, &tests_artifact, "refresh");
    for required in [
        ".p0.0",
        ".p0.1",
        ".p1.0",
        "managed.root.reload",
        "managed.root.rebuild",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            refresh.contains(required),
            "inout product omitted `{required}`:\n{refresh}"
        );
    }
    let retain_across_cleanup =
        emitted_lcir_function(&native.ir, &tests_artifact, "retainAcrossCleanup");
    for required in [
        ".p0.0",
        ".p0.1",
        ".p1.0",
        "managed.root.reload",
        "managed.root.rebuild",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            retain_across_cleanup.contains(required),
            "cleanup-crossing return product omitted `{required}`:\n{retain_across_cleanup}"
        );
    }
    assert!(
        retain_across_cleanup.contains("call ptr @loom.lcir.fn."),
        "deferred collecting call is missing:\n{retain_across_cleanup}"
    );
    assert!(
        retain_across_cleanup.contains("managed.root.rebuild")
            && retain_across_cleanup.contains("ret"),
        "the return product must be rebuilt after deferred forced collection:\n{retain_across_cleanup}"
    );
    assert!(dump.contains("inout=[0]"), "{dump}");
    let pointer_free = emitted_lcir_function(&native.ir, &tests_artifact, "pointerFree");
    for forbidden in [
        "managed.root",
        "loom_gc_",
        "loom_runtime_",
        "loom_executor_",
        "%loom.Value",
    ] {
        assert!(
            !pointer_free.contains(forbidden),
            "pointer-free product exposed `{forbidden}`:\n{pointer_free}"
        );
    }
    for forbidden in ["loom_gc_root_push_v1", "loom_executor_", "%loom.Value"] {
        assert!(!native.ir.contains(forbidden), "{}", native.ir);
    }

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create managed-product target directory");
        let object = directory.path().join("managed-products.o");
        let ir_path = directory.path().join("managed-products.ll");
        emit_lcir_native_object(
            &tests_artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit managed-product object for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read managed-product target IR");
        assert!(ir.contains("managed.root.rebuild"), "{ir}");
        assert!(ir.contains("loom_gc_typed_root_push_v1"), "{ir}");
        assert!(!ir.contains("loom_gc_root_push_v1"), "{ir}");
        assert!(!ir.contains("loom_executor_"), "{ir}");
        assert!(!ir.contains("%loom.Value"), "{ir}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one adversarial gate keeps Option, Result-product, tagless and nested sums, phi/call/inout/match relocation, guarded carrier decoding, and cross-target evidence together"
)]
fn managed_sum_leaves_relocate_only_for_the_active_variant() {
    let pressure = "x".repeat(40 * 1024);
    let source = include_str!("../../../fixtures/lcir-managed-sums/main.loom").replace(
        "fn collectPressure() Text { join(\"small\", \"pressure\") }",
        &format!("fn collectPressure() Text {{ join(\"{pressure}\", \"{pressure}\") }}"),
    );
    let program = compile_source(&source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(artifact.program());
    assert!(dump.contains("contract PreconditionFault"), "{dump}");
    assert!(dump.contains("contract PostconditionFault"), "{dump}");
    let interpreted = Interpreter::new(&program).run_tests();
    assert!(
        interpreted
            .iter()
            .all(|test| test.status == TestStatus::Passed),
        "{interpreted:?}"
    );

    let native = emit_and_run_lcir(&artifact, "source-managed-sums");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(
        String::from_utf8_lossy(&native.output.stdout).contains("managedSums"),
        "{:?}",
        native.output
    );
    for forbidden in ["loom_gc_root_push_v1", "loom_executor_", "%loom.Value"] {
        assert!(!native.ir.contains(forbidden), "{}", native.ir);
    }

    let retain_option = emitted_lcir_function(&native.ir, &artifact, "retainOption");
    for required in [
        ".s1f0",
        "managed.root.reload",
        "managed.root.rebuild.active.sum",
        "managed.root.sum.variant.active",
        "managed.root.active.pointer",
    ] {
        assert!(
            retain_option.contains(required),
            "Option[Text] root flow omitted `{required}`:\n{retain_option}"
        );
    }

    let retain_contract = emitted_lcir_function(&native.ir, &artifact, "retainContract");
    for required in ["sum.switch.tag", "text.compare.same_length"] {
        assert!(
            retain_contract.contains(required),
            "Text-bearing contract match omitted `{required}`:\n{retain_contract}"
        );
    }
    let verify = emitted_lcir_function(&native.ir, &artifact, "verify");
    for required in [
        ".s1f0",
        "managed.root.sum.variant.active",
        "managed.root.reload",
        "managed.root.rebuild.active.sum",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            verify.contains(required),
            "forced-GC contract argument omitted `{required}`:\n{verify}"
        );
    }

    let retain_result = emitted_lcir_function(&native.ir, &artifact, "retainResult");
    for required in [
        ".s0f0.p0",
        ".s0f0.p1",
        "managed.root.reload",
        "managed.root.rebuild.sum.payload",
    ] {
        assert!(
            retain_result.contains(required),
            "Result[Pair, Problem] root flow omitted `{required}`:\n{retain_result}"
        );
    }

    let nested_pair = emitted_lcir_function(&native.ir, &artifact, "nestedPair");
    for required in [
        ".s0f0.s1f0",
        ".s1f0.s0f0.p0",
        ".s1f0.s0f0.p1",
        ".s2f0",
        ".s2f1",
        "managed.root.sum.path.active",
        "managed.root.sum.safe.carrier",
        "managed.root.rebuild.sum.safe.carrier",
        "ptrtoint ptr",
        "inttoptr i64",
    ] {
        assert!(
            nested_pair.contains(required),
            "nested managed sum omitted `{required}`:\n{nested_pair}"
        );
    }
    assert!(
        nested_pair.contains("and i1"),
        "nested candidate predicates must be conjoined:\n{nested_pair}"
    );
    assert!(
        nested_pair.contains("ptr null"),
        "inactive candidates must publish null:\n{nested_pair}"
    );
    assert!(
        nested_pair.contains("zeroinitializer"),
        "inactive or malformed tags must decode only a zero carrier:\n{nested_pair}"
    );

    let tagless = emitted_lcir_function(&native.ir, &artifact, "retainEnvelope");
    assert!(tagless.contains(".s0f0"), "{tagless}");
    assert!(
        !tagless.contains("managed.root.sum.variant.active"),
        "a tagless one-variant sum must not invent a discriminant:\n{tagless}"
    );

    let inout = emitted_lcir_function(&native.ir, &artifact, "relocate");
    for required in [
        "managed.root",
        "managed.root.reload",
        "managed.root.rebuild",
        "direct.call",
    ] {
        assert!(
            inout.contains(required),
            "managed-sum inout flow omitted `{required}`:\n{inout}"
        );
    }

    let pointer_free = emitted_lcir_function(&native.ir, &artifact, "pointerFree");
    for forbidden in [
        "managed.root",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            !pointer_free.contains(forbidden),
            "pointer-free sum allocated a typed frame via `{forbidden}`:\n{pointer_free}"
        );
    }

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create managed-sum target directory");
        let object = directory.path().join("managed-sums.o");
        let ir_path = directory.path().join("managed-sums.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit managed-sum object for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read managed-sum target IR");
        for required in [
            "managed.root.sum.safe.carrier",
            "managed.root.rebuild.active.sum",
            "ptrtoint ptr",
            "inttoptr i64",
            "loom_gc_typed_root_push_v1",
        ] {
            assert!(
                ir.contains(required),
                "{target} omitted `{required}`:\n{ir}"
            );
        }
        assert!(!ir.contains("%loom.Value"), "{ir}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one native differential gate keeps managed List element shapes, immutable aliasing, List.get matching, forced relocation, unused-capacity zeroing, and cross-target descriptors together"
)]
fn managed_lists_use_precise_repeated_descriptors_and_survive_forced_relocation() {
    let fields = (0..31)
        .map(|index| format!("    n{index} Int"))
        .collect::<Vec<_>>()
        .join("\n");
    let initializers = (0..31)
        .map(|index| format!("n{index} = {index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let repeated = std::iter::repeat_n("wide", 129)
        .collect::<Vec<_>>()
        .join(", ");
    let pressure = format!(
        "record Wide {{\n    text Text\n{fields}\n}}\n\nfn forcedLists() Bool {{\n    let kept = join(\"Rel\", \"ocated\")\n    let wide = Wide {{ text = kept, {initializers} }}\n    var values = [{repeated}]\n    let alias = values\n    values.add(wide)\n    let trigger = [{repeated}]\n    (trigger.length() == 129\n        && values.length() == 130\n        && alias.length() == 129\n        && match values.get(129) {{ Some(item) => item.text == \"Relocated\", None => false }}\n        && match alias.get(0) {{ Some(item) => item.text == \"Relocated\", None => false }})\n}}\n\nfn uniqueForcedLists() Bool {{\n    let kept = join(\"Uni\", \"que\")\n    let wide = Wide {{ text = kept, {initializers} }}\n    var values = List[Wide]()\n    for index in 0..130 {{\n        values.add(wide)\n        Unit\n    }}\n    (values.length() == 130\n        && match values.get(0) {{ Some(item) => item.text == \"Unique\", None => false }}\n        && match values.get(129) {{ Some(item) => item.text == \"Unique\", None => false }})\n}}\n\n"
    );
    let source = include_str!("../../../fixtures/lcir-managed-lists/main.loom")
        .replace(
            "fn join(left Text, right Text) Text",
            &(pressure + "fn join(left Text, right Text) Text"),
        )
        .replace(
            "    verdict(\n",
            "    verdict(\n        forcedLists()\n        && uniqueForcedLists()\n        && ",
        );
    let program = compile_source(&source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let interpreted = Interpreter::new(&program).run_tests();
    assert!(
        interpreted
            .iter()
            .all(|test| test.status == TestStatus::Passed),
        "{interpreted:?}"
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(artifact.program());
    for required in [
        "list.construct",
        "list.append",
        "list.append.unique",
        "list.length",
        "list.get",
        "sum.switch",
        "inout=[0]",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }

    let native = emit_and_run_lcir(&artifact, "source-managed-lists");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(
        String::from_utf8_lossy(&native.output.stdout).contains("managedLists"),
        "{:?}",
        native.output
    );
    for required in [
        "loom_gc_typed_repeated_alloc_v1",
        "loom.lcir.list.descriptor",
        "loom.lcir.list.pointer_offsets",
        "llvm.memcpy",
        "managed.root.reload",
        "list.append.copy_bytes",
        "list.get.in_bounds",
    ] {
        assert!(
            native.ir.contains(required),
            "managed List IR omitted `{required}`:\n{}",
            native.ir
        );
    }
    for forbidden in [
        "%loom.Value",
        "loom_runtime_list_add",
        "loom_runtime_list_get",
        "loom_int_list_reserve_v1",
        "loom_executor_",
        "loom_gc_root_push_v1",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "managed List IR exposed `{forbidden}`:\n{}",
            native.ir
        );
    }

    let unique = emitted_lcir_function(&native.ir, &artifact, "uniqueForcedLists");
    for required in [
        "list.append.unique.can_reuse",
        "list.append.unique.reuse",
        "list.append.unique.grow",
        "managed.root.reload",
    ] {
        assert!(unique.contains(required), "missing `{required}`:\n{unique}");
    }
    assert_eq!(
        unique.matches("@loom_gc_typed_repeated_alloc_v1").count(),
        1,
        "one loop append site must contain one conditional allocator call:\n{unique}"
    );
    let shared = emitted_lcir_function(&native.ir, &artifact, "forcedLists");
    assert!(
        !shared.contains("list.append.unique.reuse"),
        "the aliased append must remain immutable:\n{shared}"
    );

    let release_directory = tempfile::tempdir().expect("create release List directory");
    let release_object = release_directory.path().join("managed-lists-release.o");
    let release_ir_path = release_directory.path().join("managed-lists-release.ll");
    emit_lcir_native_object(
        &artifact,
        &release_object,
        &NativeObjectOptions {
            emit_ir: Some(release_ir_path.clone()),
            optimization: OptimizationProfile::Release,
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit release managed-List object");
    let release_ir = std::fs::read_to_string(release_ir_path).expect("read release List IR");
    let release_unique = emitted_lcir_function(&release_ir, &artifact, "uniqueForcedLists");
    assert_eq!(
        release_unique
            .matches("@loom_gc_typed_repeated_alloc_v1")
            .count(),
        1,
        "release loop must retain one conditional allocator call site:\n{release_unique}"
    );
    assert!(
        release_unique.contains("list.append.unique.can_reuse")
            || (release_unique.contains("icmp") && release_unique.contains("br i1")),
        "release IR lost the capacity reuse guard:\n{release_unique}"
    );

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create managed-List target directory");
        let object = directory.path().join("managed-lists.o");
        let ir_path = directory.path().join("managed-lists.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit managed-List object for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read managed-List target IR");
        for required in [
            "loom_gc_typed_repeated_alloc_v1",
            "loom.lcir.list.descriptor",
            "managed.root.reload",
            "llvm.memcpy",
        ] {
            assert!(
                ir.contains(required),
                "{target} omitted `{required}`:\n{ir}"
            );
        }
        assert!(!ir.contains("%loom.Value"), "{ir}");
    }
}

#[test]
fn managed_list_source_is_direct_on_64_bit_and_fails_closed_on_32_bit() {
    let program = compile_source(
        "module list_target\npub fn main() Unit {\n    let values = [1, 2]\n    discard values.length()\n    Unit\n}\n",
    );
    let request = SourceArtifactRequest::Run {
        entry: "main".into(),
    };
    let artifact = lower_source_artifact_with_layout(
        &program,
        &request,
        TargetLayout::new(64).expect("64-bit target"),
    );
    assert!(
        dump_program(artifact.program()).contains("list.construct"),
        "64-bit List[Int] must remain direct LCIR"
    );
    match lower_typed_artifact(
        &program,
        &request,
        TargetLayout::new(32).expect("32-bit target"),
    )
    .expect("classify 32-bit List")
    {
        LoweringOutcome::Unsupported(report) => assert!(
            report.items().iter().any(|item| {
                matches!(
                    item.feature(),
                    loom_codegen_ir::UnsupportedFeature::ExpressionType
                        | loom_codegen_ir::UnsupportedFeature::SignatureType
                )
            }),
            "{report:?}"
        ),
        LoweringOutcome::Complete(_) => panic!("32-bit managed List must fail closed"),
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
fn generic_products_and_proven_wrappers_execute_through_typed_lcir() {
    let source = include_str!("../../../fixtures/lcir-generic-products/main.loom");
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
        "Nominal#",
        "[Int]",
        "[Text]",
        "product.construct",
        "invariant_record.proven",
        "refine.proven",
        "unrefine",
    ] {
        assert!(dump.contains(expected), "missing `{expected}`:\n{dump}");
    }

    let native = emit_and_run_lcir_with_options(
        &artifact,
        "source-generic-products",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );
    let legacy = emit_and_run_legacy(&program, "main", "legacy-generic-products");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(native.output.stdout, legacy.stdout);
    assert_eq!(native.output.stderr, legacy.stderr);
    assert!(!native.ir.contains("%loom.Value"), "{}", native.ir);
    assert!(!native.ir.contains("loom_executor_"), "{}", native.ir);

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create generic-product target output");
        let object = directory.path().join(if target.contains("windows") {
            "generic-products.obj"
        } else {
            "generic-products.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit generic products for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
    }
}

#[test]
fn structural_equality_executes_products_sums_contracts_and_lists_through_typed_lcir() {
    let source = include_str!("../../../fixtures/lcir-structural-equality/main.loom");
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
        "product.extract",
        "sum.switch",
        "unrefine",
        "list.length",
        "list.get",
        "int.successor_below",
        "text.compare.equal",
        "bool.not",
    ] {
        assert!(dump.contains(expected), "missing `{expected}`:\n{dump}");
    }

    let native = emit_and_run_lcir_with_options(
        &artifact,
        "source-structural-equality",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );
    let legacy = emit_and_run_legacy(&program, "main", "legacy-structural-equality");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(native.output.stdout, legacy.stdout);
    assert_eq!(native.output.stderr, legacy.stderr);
    for forbidden in [
        "%loom.Value",
        "@loom.fn.",
        "loom_gc_root_push_v1",
        "loom_gc_root_pop_v1",
        "loom_executor_",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "unexpected `{forbidden}`:\n{}",
            native.ir
        );
    }
    assert!(
        native.ir.contains("loom_gc_typed_root_push_v1"),
        "{}",
        native.ir
    );
    assert!(
        native.ir.contains("loom_gc_typed_root_pop_v1"),
        "{}",
        native.ir
    );
    assert_no_indirect_calls(&native.ir);

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create structural-equality target output");
        let object = directory.path().join(if target.contains("windows") {
            "structural-equality.obj"
        } else {
            "structural-equality.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit structural equality for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
    }
}

fn static_concepts_test_artifact() -> CheckedArtifact {
    let source = include_str!("../../../fixtures/lcir-static-concepts/main.loom");
    let program = compile_source(source);
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].name, "lcir_static_concepts.staticConcepts",
        "{interpreted:#?}"
    );
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(artifact.program());
    assert!(dump.contains("witnesses=[Apply#"), "{dump}");
    assert!(dump.contains("witnesses=[Concrete#"), "{dump}");
    assert!(!dump.contains("Projection#"), "{dump}");
    artifact
}

#[test]
fn static_concepts_run_directly_on_host_without_runtime_witnesses() {
    let artifact = static_concepts_test_artifact();
    let native = emit_and_run_lcir(&artifact, "source-static-concepts");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(
        String::from_utf8_lossy(&native.output.stdout)
            .contains("passed lcir_static_concepts.staticConcepts"),
        "{:?}",
        native.output
    );
    assert_no_legacy_surface(&native.ir);
    assert_no_indirect_calls(&native.ir);
    for forbidden in [
        "loom_runtime_create_v1",
        "loom_runtime_activate_v1",
        "loom_gc_",
        "loom_executor_",
        "WitnessInstance",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "unexpected `{forbidden}`:\n{}",
            native.ir
        );
    }
}

#[test]
fn static_concepts_emit_direct_msvc_object_without_runtime_witnesses() {
    let artifact = static_concepts_test_artifact();
    let directory = tempfile::tempdir().expect("create MSVC static-concept directory");
    let object = directory.path().join("static-concepts.obj");
    let ir_path = directory.path().join("static-concepts-msvc.ll");
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
    .expect("emit direct static-concept MSVC object");
    let object_bytes = std::fs::read(&object).expect("read MSVC static-concept object");
    assert_eq!(
        object_bytes.get(..2),
        Some([0x64, 0x86].as_slice()),
        "x86_64 MSVC output must be a real AMD64 COFF object"
    );
    let ir = std::fs::read_to_string(ir_path).expect("read MSVC static-concept IR");
    assert!(
        ir.contains("target triple = \"x86_64-pc-windows-msvc\""),
        "{ir}"
    );
    assert_no_legacy_surface(&ir);
    assert_no_indirect_calls(&ir);
    for forbidden in [
        "loom_runtime_create_v1",
        "loom_runtime_activate_v1",
        "loom_gc_",
        "loom_executor_",
        "WitnessInstance",
    ] {
        assert!(!ir.contains(forbidden), "unexpected `{forbidden}`:\n{ir}");
    }
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
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate keeps all lexical cleanup exit shapes and exact fault metadata together"
)]
fn typed_lexical_cleanup_matches_interpreter_and_legacy_on_every_exit_shape() {
    let source = r"module typed_lexical_cleanup

fn requireEqual(actual Int, expected Int) Unit {
    assert actual == expected
    Unit
}

pub fn normalMain() Unit {
    var order = 0
    {
        defer {
            order = order * 10 + 1
        }
        defer {
            order = order * 10 + 2
        }
        Unit
    }
    requireEqual(order, 21)
    if true {
        defer {
            order = 34
        }
        Unit
    } else {
        order = 99
        Unit
    }
    requireEqual(order, 34)
    Unit
}

pub fn earlyReturnMain() Unit {
    defer {
        assert false
        Unit
    }
    return
}

pub fn bodyFaultMain() Unit {
    defer {
        assert false
        Unit
    }
    let primary = 1 / 0
    discard primary
    Unit
}

pub fn cleanupFaultMain() Unit {
    defer {
        let secondary = 1 / 0
        discard secondary
        Unit
    }
    defer {
        assert false
        Unit
    }
    Unit
}
";
    let program = compile_source(source);
    {
        let entry = "normalMain";
        assert_eq!(interpret_run(&program, entry), Ok(Value::Unit));
        let artifact = lower_source_artifact(
            &program,
            &SourceArtifactRequest::Run {
                entry: entry.into(),
            },
        );
        let lcir = emit_and_run_lcir_machine_fault(&artifact, &format!("lcir-cleanup-{entry}"));
        let legacy =
            emit_and_run_legacy_machine_fault(&program, entry, &format!("legacy-cleanup-{entry}"));
        assert!(lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert!(legacy.status.success(), "{entry}: {legacy:?}");
        assert_eq!(lcir.output.stdout, legacy.stdout, "{entry}");
        assert_eq!(lcir.output.stderr, legacy.stderr, "{entry}");
        assert_fallible_surface(&lcir.ir);
    }

    for (entry, expected) in [
        ("earlyReturnMain", "AssertionFault"),
        ("bodyFaultMain", "IntegerDivisionByZero"),
        ("cleanupFaultMain", "AssertionFault"),
    ] {
        let interpreted = interpret_run(&program, entry).expect_err("interpreter cleanup fault");
        let interpreted = serde_json::to_value(interpreted).expect("serialize cleanup fault");
        assert_eq!(interpreted["fault"]["code"], expected, "{entry}");
        let artifact = lower_source_artifact(
            &program,
            &SourceArtifactRequest::Run {
                entry: entry.into(),
            },
        );
        let lcir = emit_and_run_lcir_machine_fault(&artifact, &format!("lcir-cleanup-{entry}"));
        let legacy =
            emit_and_run_legacy_machine_fault(&program, entry, &format!("legacy-cleanup-{entry}"));
        assert!(!lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert!(!legacy.status.success(), "{entry}: {legacy:?}");
        let lcir_fault = machine_fault(&lcir.output);
        let legacy_fault = machine_fault(&legacy);
        let code = |fault: &serde_json::Value| {
            fault["fault"]["code"]
                .as_str()
                .or_else(|| fault["code"].as_str())
                .map(str::to_owned)
        };
        assert_eq!(code(&lcir_fault).as_deref(), Some(expected), "LCIR {entry}");
        assert_eq!(
            code(&legacy_fault).as_deref(),
            Some(expected),
            "legacy {entry}"
        );
        if expected == "AssertionFault" {
            assert_eq!(legacy_fault, interpreted, "legacy metadata {entry}");
            assert_eq!(lcir_fault, interpreted, "LCIR assertion metadata {entry}");
        } else {
            assert_eq!(
                lcir_fault["sourceSpan"]["file"], interpreted["fault"]["span"]["file"],
                "LCIR source file {entry}"
            );
            assert_eq!(
                lcir_fault["sourceSpan"]["start"], interpreted["fault"]["span"]["range"]["start"],
                "LCIR source start {entry}"
            );
            assert_eq!(
                lcir_fault["sourceSpan"]["end"], interpreted["fault"]["span"]["range"]["end"],
                "LCIR source end {entry}"
            );
        }
        assert_fallible_surface(&lcir.ir);
    }
}

#[test]
fn typed_scoped_disposal_is_one_static_inout_call_after_initialization() {
    let source = r"module standard.resource

concept Dispose {
    method dispose(mut self) Unit
}

concept MustScope {}
concept NoSuspend {}

record Resource {
    value Int
}

impl Dispose for Resource {
    method dispose(mut self) Unit {
        let acquired = self.value
        assert acquired > 0
        self.value = 0
        Unit
    }
}

impl MustScope for Resource {}

pub fn successMain() Unit {
    scoped resource = Resource { value = 3 }
    Unit
}

pub fn disposalFaultMain() Unit {
    scoped resource = Resource { value = 0 }
    Unit
}

pub fn initializerFaultMain() Unit {
    scoped resource = Resource { value = 1 / 0 }
    Unit
}
";
    let program = compile_source(source);
    for (entry, expected_fault) in [
        ("successMain", None),
        ("disposalFaultMain", Some("AssertionFault")),
        ("initializerFaultMain", Some("IntegerDivisionByZero")),
    ] {
        let succeeds = expected_fault.is_none();
        let interpreted = interpret_run(&program, entry);
        assert_eq!(interpreted.is_ok(), succeeds, "interpreter {entry}");
        let artifact = lower_source_artifact(
            &program,
            &SourceArtifactRequest::Run {
                entry: entry.into(),
            },
        );
        let dump = dump_program(artifact.program());
        assert_eq!(dump.matches("invoke i0").count(), 1, "{entry}: {dump}");
        if entry == "initializerFaultMain" {
            let fault_target = dump
                .lines()
                .find(|line| line.contains("checked_int.divide"))
                .and_then(|line| line.split("fault b").nth(1))
                .and_then(|suffix| suffix.split('(').next())
                .unwrap_or_else(|| panic!("missing initializer fault edge: {dump}"));
            let block = dump
                .split(&format!("  b{fault_target}:"))
                .nth(1)
                .and_then(|suffix| suffix.split("\n\n").next())
                .unwrap_or_else(|| {
                    panic!("missing initializer fault block b{fault_target}: {dump}")
                });
            assert!(block.contains("resume_fault"), "{block}\n{dump}");
            assert!(!block.contains("invoke"), "{block}\n{dump}");
        }
        let lcir = emit_and_run_lcir_machine_fault(&artifact, &format!("lcir-scoped-{entry}"));
        let legacy =
            emit_and_run_legacy_machine_fault(&program, entry, &format!("legacy-scoped-{entry}"));
        assert_eq!(
            lcir.output.status.success(),
            succeeds,
            "{entry}: {:?}",
            lcir.output
        );
        assert_eq!(legacy.status.success(), succeeds, "{entry}: {legacy:?}");
        assert_eq!(lcir.output.stdout, legacy.stdout, "{entry}");
        if succeeds {
            assert_eq!(lcir.output.stderr, legacy.stderr, "{entry}");
        } else {
            let fault_code = |fault: &serde_json::Value| {
                fault["fault"]["code"]
                    .as_str()
                    .or_else(|| fault["code"].as_str())
                    .map(str::to_owned)
            };
            assert_eq!(
                fault_code(&machine_fault(&lcir.output)).as_deref(),
                expected_fault,
                "LCIR {entry}"
            );
            assert_eq!(
                fault_code(&machine_fault(&legacy)).as_deref(),
                expected_fault,
                "legacy {entry}"
            );
        }
        assert_fallible_surface(&lcir.ir);
        assert_no_indirect_calls(&lcir.ir);
    }
}

#[test]
fn typed_builtin_scoped_cleanup_matches_interpreter_and_legacy_without_executor_routing() {
    let program = checked_builtin_file_cleanup_fixture();
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert_eq!(dump.matches("resource.close.file").count(), 1, "{dump}");
    let lcir = emit_and_run_lcir_machine_fault(&artifact, "lcir-scoped-file-close");
    let legacy = emit_and_run_legacy_machine_fault(&program, "main", "legacy-scoped-file-close");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    assert_eq!(lcir.output.stderr, legacy.stderr);
    assert!(
        lcir.ir.contains("@loom_runtime_resource_close_typed_v1"),
        "{}",
        lcir.ir
    );
    assert_eq!(
        lcir.ir
            .matches("call i32 @loom_runtime_resource_close_typed_v1")
            .count(),
        1,
        "{}",
        lcir.ir
    );
    assert_fallible_surface(&lcir.ir);
    assert_no_indirect_calls(&lcir.ir);
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
fn contract_int_negation_overflow_matches_interpreter_lcir_and_legacy() {
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
        let artifact = lower_source_artifact(
            &program,
            &SourceArtifactRequest::Run {
                entry: entry.into(),
            },
        );
        let lcir = emit_and_run_lcir_machine_fault(
            &artifact,
            &format!("lcir-contract-int-negation-{entry}"),
        );
        assert!(!lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert_eq!(machine_fault(&lcir.output), expected, "LCIR {entry}");
        assert!(!legacy.status.success(), "{entry}: {legacy:?}");
        assert_eq!(machine_fault(&legacy), expected, "legacy {entry}");
    }
}

#[test]
fn checked_contract_binary_overflow_and_short_circuit_match_all_backends() {
    let source = r"module contract_checked_binary

fn overflow(value Int) Unit
    requires value + 1 > 0
{
    Unit
}

fn shortCircuit(value Int) Unit
    requires true || value + 1 > 0
{
    Unit
}

pub fn overflowMain() Unit {
    overflow(9223372036854775807)
    Unit
}

pub fn shortCircuitMain() Unit {
    shortCircuit(9223372036854775807)
    Unit
}
";
    let program = compile_source(source);
    let overflow = source_function(&program, "overflow");
    let operation = match &overflow.call_plan.requires[0].expression.kind {
        ContractExprKind::Binary(_, left, _) => left.as_ref(),
        other => panic!("unexpected checked contract: {other:?}"),
    };
    let expected = integer_overflow_fault(&operation.span);
    assert_eq!(
        serde_json::to_value(
            interpret_run(&program, "overflowMain").expect_err("contract overflow")
        )
        .expect("serialize contract overflow"),
        expected
    );
    let overflow_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "overflowMain".into(),
        },
    );
    let overflow_lcir =
        emit_and_run_lcir_machine_fault(&overflow_artifact, "lcir-contract-binary-overflow");
    let overflow_legacy = emit_and_run_legacy_machine_fault(
        &program,
        "overflowMain",
        "legacy-contract-binary-overflow",
    );
    assert_eq!(machine_fault(&overflow_lcir.output), expected);
    assert_eq!(machine_fault(&overflow_legacy), expected);

    assert_eq!(interpret_run(&program, "shortCircuitMain"), Ok(Value::Unit));
    let safe_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "shortCircuitMain".into(),
        },
    );
    let safe_lcir = emit_and_run_lcir_machine_fault(&safe_artifact, "lcir-contract-short-circuit");
    let safe_legacy = emit_and_run_legacy_machine_fault(
        &program,
        "shortCircuitMain",
        "legacy-contract-short-circuit",
    );
    assert!(safe_lcir.output.status.success(), "{:?}", safe_lcir.output);
    assert!(safe_legacy.status.success(), "{safe_legacy:?}");
    assert_eq!(safe_lcir.output.stdout, safe_legacy.stdout);
}

#[test]
fn contract_precondition_blame_matches_each_closed_world_call_and_checked_root() {
    let source = r"module contract_blame

fn positive(value Int) Unit
    requires value > 0
{
    Unit
}

fn allArgumentsBeforeRequires(first Int, later Int) Unit
    requires first > 0
{
    discard later
    Unit
}

pub fn callerMain() Unit {
    positive(0)
    Unit
}

pub fn rootMain() Unit
    requires false
{
    Unit
}

pub fn argumentFaultMain() Unit {
    allArgumentsBeforeRequires(0, 1 / 0)
}
";
    let program = compile_source(source);

    for entry in ["callerMain", "rootMain"] {
        let interpreted =
            serde_json::to_value(interpret_run(&program, entry).expect_err("contract must reject"))
                .expect("serialize interpreter contract fault");
        let artifact = lower_source_artifact(
            &program,
            &SourceArtifactRequest::Run {
                entry: entry.into(),
            },
        );
        let dump = dump_program(artifact.program());
        if entry == "rootMain" {
            assert!(dump.contains("checked-root source="), "{dump}");
        }
        let lcir = emit_and_run_lcir_machine_fault(&artifact, &format!("lcir-contract-{entry}"));
        let legacy =
            emit_and_run_legacy_machine_fault(&program, entry, &format!("legacy-contract-{entry}"));
        assert!(!lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert!(!legacy.status.success(), "{entry}: {legacy:?}");
        assert_eq!(machine_fault(&lcir.output), interpreted, "LCIR {entry}");
        assert_eq!(machine_fault(&legacy), interpreted, "legacy {entry}");
    }

    let caller = source_function(&program, "callerMain");
    let call_span = caller
        .body
        .statements
        .iter()
        .find_map(|statement| match &statement.kind {
            StatementKind::Evaluate(Expr {
                kind: ExprKind::Call { .. },
                span,
                ..
            }) => Some(*span),
            _ => None,
        })
        .expect("caller expression span");
    let failure = serde_json::to_value(
        interpret_run(&program, "callerMain").expect_err("caller precondition fault"),
    )
    .expect("serialize caller precondition fault");
    assert_eq!(failure["fault"]["blameSpan"], serde_json::json!(call_span));
    assert_ne!(call_span, caller.span);

    let argument_failure = serde_json::to_value(
        interpret_run(&program, "argumentFaultMain")
            .expect_err("the later argument must fault before the precondition"),
    )
    .expect("serialize argument-evaluation fault");
    assert_eq!(argument_failure["fault"]["code"], "IntegerDivisionByZero");
    let argument_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "argumentFaultMain".into(),
        },
    );
    let argument_lcir =
        emit_and_run_lcir_machine_fault(&argument_artifact, "lcir-contract-argument-order");
    let argument_legacy = emit_and_run_legacy_machine_fault(
        &program,
        "argumentFaultMain",
        "legacy-contract-argument-order",
    );
    assert_eq!(
        machine_fault(&argument_lcir.output)["code"],
        argument_failure["fault"]["code"]
    );
    assert_eq!(
        machine_fault(&argument_legacy)["fault"]["code"],
        argument_failure["fault"]["code"]
    );
}

#[test]
fn mutable_receiver_old_current_and_cleanup_order_match_all_backends() {
    let source = r"module contract_mutable_receiver

record Boxed { value Int }

fn requireEqual(actual Int, expected Int) Unit {
    assert actual == expected
    Unit
}

impl Boxed {
    method replaceAfterCleanup(mut self, target Int) Unit
        ensures old(self.value) == 1
        ensures self.value == target
    {
        self.value = 0
        defer {
            self.value = target
        }
        return Unit
    }
}

record Counter {
    value Int
    invariant self.value >= 0
}

impl Counter {
    method increase(mut self, amount Int) Unit
        requires amount >= 0
        ensures old(self.value) == 2
        ensures self.value == 5
    {
        self.value = self.value + amount
        Unit
    }
}

pub fn main() Unit {
    var boxed = Boxed { value = 1 }
    boxed.replaceAfterCleanup(7)
    requireEqual(boxed.value, 7)

    var counter = Counter { value = 2 }
    counter.increase(3)
    requireEqual(counter.value, 5)
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
    let dump = dump_program(artifact.program());
    assert!(dump.contains("contract PostconditionFault"), "{dump}");
    assert!(dump.contains("contract InvariantFault"), "{dump}");
    assert!(dump.contains("invariant_receiver.insert"), "{dump}");
    assert!(dump.contains("writebacks"), "{dump}");

    let lcir = emit_and_run_lcir_machine_fault(&artifact, "lcir-contract-mutable-receiver");
    let legacy =
        emit_and_run_legacy_machine_fault(&program, "main", "legacy-contract-mutable-receiver");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    assert_eq!(lcir.output.stderr, legacy.stderr);
    assert!(!lcir.ir.contains("loom.Value"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("loom_executor_"), "{}", lcir.ir);
}

#[test]
fn cleanup_fault_precedes_the_postcondition_and_matches_all_backends() {
    let source = r"module contract_cleanup_fault

fn failDuringCleanup() Unit
    ensures false
{
    defer {
        discard 1 / 0
    }
    Unit
}

pub fn main() Unit {
    failDuringCleanup()
}
";
    let program = compile_source(source);
    let interpreted = serde_json::to_value(
        interpret_run(&program, "main").expect_err("cleanup must fault before the postcondition"),
    )
    .expect("serialize cleanup fault");
    assert_eq!(interpreted["fault"]["code"], "IntegerDivisionByZero");

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("contract PostconditionFault"), "{dump}");
    assert!(dump.contains("checked_int.divide"), "{dump}");
    let lcir = emit_and_run_lcir_machine_fault(&artifact, "lcir-contract-cleanup-fault");
    let legacy =
        emit_and_run_legacy_machine_fault(&program, "main", "legacy-contract-cleanup-fault");
    let lcir_fault = machine_fault(&lcir.output);
    assert_eq!(lcir_fault["code"], interpreted["fault"]["code"]);
    assert_eq!(
        lcir_fault["sourceSpan"]["file"],
        interpreted["fault"]["span"]["file"]
    );
    assert_eq!(
        lcir_fault["sourceSpan"]["start"],
        interpreted["fault"]["span"]["range"]["start"]
    );
    assert_eq!(
        lcir_fault["sourceSpan"]["end"],
        interpreted["fault"]["span"]["range"]["end"]
    );
    let legacy_fault = machine_fault(&legacy);
    assert_eq!(legacy_fault["fault"]["code"], interpreted["fault"]["code"]);
    assert_eq!(
        legacy_fault["fault"]["message"],
        interpreted["fault"]["message"]
    );
    assert!(!lcir.ir.contains("loom.Value"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("loom_executor_"), "{}", lcir.ir);
}

#[test]
fn nested_contract_matches_and_static_concept_calls_match_all_backends() {
    let source = r"module contract_static_match

concept Source {
    method first(self, allowed Bool) Option[Int]
        requires allowed
        ensures match result {
            Some(value) => value >= 0
            None => true
        }
}

record Number { value Int }

impl Source for Number {
    method first(self, allowed Bool) Option[Int] {
        if allowed { Some(self.value) } else { None }
    }
}

fn read[T: Source](source T) Bool {
    match source.first(true) {
        Some(value) => value == 7
        None => false
    }
}

enum Problem { Failed }

fn keep(value Option[Int]) Result[Option[Int], Problem]
    ensures match result {
        Ok(option) => match option {
            Some(number) => number >= 0
            None => true
        }
        Err(_) => true
    }
{
    Ok(value)
}

pub fn main() Unit {
    let staticOk = read(Number { value = 7 })
    let nestedOk = match keep(Some(3)) {
        Ok(Some(number)) => number == 3
        Ok(None) => false
        Err(_) => false
    }
    if staticOk && nestedOk { Unit } else {
        discard 1 / 0
        Unit
    }
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
    let dump = dump_program(artifact.program());
    assert!(dump.matches("sum.switch").count() >= 4, "{dump}");
    assert!(dump.contains("contract PreconditionFault"), "{dump}");
    assert!(dump.contains("contract PostconditionFault"), "{dump}");
    assert!(dump.contains("witnesses=[Concrete#"), "{dump}");

    let lcir = emit_and_run_lcir(&artifact, "lcir-contract-static-match");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-contract-static-match");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    assert!(!lcir.ir.contains("loom.Value"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("loom_executor_"), "{}", lcir.ir);
}

#[test]
fn managed_text_product_remains_typed_and_live_through_a_contract_check() {
    let source = r#"module contract_managed_product

record Label { value Text }

fn accept(label Label, pressure Text) Unit
    requires label.value == "Keep"
{
    discard pressure.length()
    Unit
}

pub fn main() Unit {
    let label = Label { value = "K".concat("eep") }
    accept(label, "x".concat("y"))
    Unit
}
"#;
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("managed_ptr"), "{dump}");
    assert!(dump.contains("text.compare.equal"), "{dump}");
    let lcir = emit_and_run_lcir(&artifact, "lcir-contract-managed-product");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-contract-managed-product");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    assert!(
        lcir.ir.contains("loom_gc_typed_root_push_v1"),
        "{}",
        lcir.ir
    );
    assert!(!lcir.ir.contains("loom_gc_root_push_v1"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("loom.Value"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("loom_executor_"), "{}", lcir.ir);
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
fn runtime_refinement_checks_return_typed_constraint_results() {
    let source = r"module lcir_source_dynamic_refined

type Money = Float where self >= 0.0

fn checked(raw Float) Result[Money, ConstraintError] {
    Money(raw)
}

pub fn main() Unit {
    let accepted = match checked(1.0) {
        Ok(value) => value == 1.0
        Err(_) => false
    }
    let rejected = match checked(-1.0) {
        Err(_) => true
        Ok(_) => false
    }
    assert accepted
    assert rejected
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
    let dump = dump_program(artifact.program());
    assert!(dump.contains("refine.proven"), "{dump}");
    assert!(dump.contains("ConstraintViolation"), "{dump}");
    assert!(dump.contains("sum.construct variant 0"), "{dump}");
    assert!(dump.contains("sum.construct variant 1"), "{dump}");

    let lcir = emit_and_run_lcir(&artifact, "runtime-refinement");
    let legacy = emit_and_run_legacy(&program, "main", "legacy-runtime-refinement");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(legacy.status.success(), "{legacy:?}");
    assert_eq!(lcir.output.stdout, legacy.stdout);
    assert_no_legacy_surface(&lcir.ir);
    assert!(!lcir.ir.contains("loom_executor_"), "{}", lcir.ir);
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
