#![allow(clippy::default_trait_access)]

use std::process::Command;

use loom_codegen_llvm::{
    EmitOptions, NativeRouteKind, NativeRoutePolicy, OptimizationProfile,
    emit_prepared_native_object, prepare_native_object,
};
use loom_core::Span;
use loom_core::runtime_fault::{
    ARTIFACT_PROOF_REJECTED_FAULT_CODE, ARTIFACT_PROOF_REJECTED_FAULT_MESSAGE,
};
use loom_driver::{AnalysisHost, PersistentCache};
use loom_interpreter::{ExecutionFailure, Interpreter, Value};
use loom_mir::{
    CheckedProgram, StatementKind, decode_interpreted_executable_artifact,
    encode_interpreted_executable_artifact,
};
use loom_runtime_abi::{FAULT_FORMAT_ENV, FAULT_FORMAT_JSON, FAULT_JSON_PREFIX};

mod support;
use support::{emit_native, link_native_object};

fn compile_source(source: &str) -> CheckedProgram {
    let project = tempfile::tempdir().expect("create proof source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write proof source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load proof source")
        .snapshot()
        .expect("analyze proof source");
    assert!(
        !snapshot.has_errors(),
        "proof source diagnostics: {:#?}",
        snapshot.diagnostics()
    );
    snapshot.executable().expect("lower checked MIR").clone()
}

fn decode_with_tampered_float(
    program: &CheckedProgram,
    entry: &str,
    original: f64,
    replacement: f64,
    forge_proven_disposition: bool,
) -> (CheckedProgram, String) {
    let encoded = encode_interpreted_executable_artifact(program, entry)
        .expect("encode proof executable artifact");
    let mut envelope: serde_json::Value = serde_json::from_slice(&encoded).expect("artifact JSON");
    let mut replaced = 0;
    for slot in envelope["floatBits"]
        .as_array_mut()
        .expect("artifact Float side table")
    {
        if slot.as_u64() == Some(original.to_bits()) {
            *slot = serde_json::json!(replacement.to_bits());
            replaced += 1;
        }
    }
    assert_eq!(
        replaced, 1,
        "construction must have one matching Float slot"
    );
    let normalized = serde_json::to_string(&envelope).expect("encode tampered JSON");
    assert!(normalized.contains("\"construction\":\"recheck\""));
    let wire = if forge_proven_disposition {
        normalized.replace(
            "\"construction\":\"recheck\"",
            "\"construction\":\"proven\"",
        )
    } else {
        normalized
    };
    decode_interpreted_executable_artifact(wire.as_bytes())
        .expect("decode tampered proof artifact through the untrusted boundary")
}

fn first_evaluated_expression_span(
    program: &CheckedProgram,
    function: loom_mir::FunctionId,
) -> Span {
    let function = &program.functions[usize::try_from(function.0).expect("function id fits usize")];
    let statement = function
        .body
        .statements
        .first()
        .expect("construction statement");
    let StatementKind::Evaluate(expression) = &statement.kind else {
        panic!(
            "expected construction evaluation, found {:?}",
            statement.kind
        );
    };
    expression.span
}

fn assert_canonical_proof_failure(failure: &ExecutionFailure, expected_span: Span) {
    let ExecutionFailure::Runtime { fault } = failure else {
        panic!("proof replay produced a non-runtime failure: {failure:?}");
    };
    assert_eq!(fault.code, ARTIFACT_PROOF_REJECTED_FAULT_CODE);
    assert_eq!(fault.message, ARTIFACT_PROOF_REJECTED_FAULT_MESSAGE);
    assert_eq!(fault.span, expected_span);
}

fn assert_fresh_proof_uses_lcir(program: &CheckedProgram, entry: &str) {
    let prepared = prepare_native_object(
        program,
        EmitOptions::run(entry).with_optimization(OptimizationProfile::Release),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare fresh process-local proof");
    assert_eq!(
        prepared.route_kind(),
        NativeRouteKind::Lcir,
        "fresh Proven must remain a zero-check direct LCIR construction"
    );
}

fn native_json_failure(
    program: &CheckedProgram,
    entry: &str,
    directory: &std::path::Path,
    stem: &str,
) -> serde_json::Value {
    let executable = directory.join(stem);
    let options = EmitOptions::run(entry).with_optimization(OptimizationProfile::Release);
    let route = emit_automatic_executable(program, &executable, options);
    assert_eq!(route, NativeRouteKind::Lcir);
    let output = Command::new(executable)
        .env(FAULT_FORMAT_ENV, FAULT_FORMAT_JSON)
        .output()
        .expect("run proof replay failure");
    assert!(!output.status.success(), "{output:?}");
    String::from_utf8(output.stderr)
        .expect("native proof stderr is UTF-8")
        .lines()
        .find_map(|line| line.strip_prefix(FAULT_JSON_PREFIX))
        .map(|line| serde_json::from_str(line).expect("native fault JSON"))
        .expect("native proof failure has a structured record")
}

fn emit_automatic_executable(
    program: &CheckedProgram,
    executable: &std::path::Path,
    options: EmitOptions,
) -> NativeRouteKind {
    let object = executable.with_extension("o");
    let prepared = prepare_native_object(program, options, NativeRoutePolicy::Automatic)
        .expect("prepare automatic proof route");
    let route = prepared.route_kind();
    emit_prepared_native_object(&prepared, &object).expect("emit prepared proof object");
    link_native_object(&object, executable).expect("link prepared proof executable");
    route
}

#[test]
fn runtime_constraint_errors_are_exact_typed_values_without_secret_disclosure() {
    let source = r#"module runtime_constraint_values

record Token {
    secret Text

    invariant self.secret == "allowed"
}

fn checked(secret Text) Result[Token, ConstraintError] {
    Token { secret = secret }
}

pub fn main() Unit {
    match checked("customer-token-do-not-disclose") {
        Err(_) => Unit
        Ok(_) => {
            assert false
            Unit
        }
    }
}
"#;
    let program = compile_source(source);
    let checked = program
        .functions
        .iter()
        .find(|function| function.name.rsplit('.').next() == Some("checked"))
        .expect("checked function");
    let token = program
        .types
        .iter()
        .find(|definition| definition.name == "Token")
        .expect("Token type");
    let loom_mir::TypeDefKind::Record {
        invariant: Some(invariant),
        ..
    } = &token.kind
    else {
        panic!("Token invariant shape changed")
    };
    let runtime_secret = "runtime-secret-never-in-source-or-diagnostics";
    let interpreted = Interpreter::new(&program)
        .invoke(
            checked.id,
            vec![Value::Text {
                value: runtime_secret.into(),
            }],
            Span::default(),
        )
        .expect("interpreter returns a rejected Result value");
    let Value::Enum {
        variant, payload, ..
    } = interpreted
    else {
        panic!("runtime constraint did not return Result: {interpreted:?}")
    };
    assert_eq!(variant, loom_mir::VariantId(1));
    let [Value::ConstraintError { value: error }] = payload.as_slice() else {
        panic!("runtime constraint Err payload is not ConstraintError: {payload:?}")
    };
    assert_eq!(error.target_type, "Token");
    assert_eq!(error.code, "InvariantViolation");
    assert_eq!(error.predicate, "Token.invariant");
    assert!(error.path.is_empty());
    assert_eq!(error.value_summary, "Token");
    assert!(!error.value_summary.contains(runtime_secret));
    assert_eq!(error.contract_span, invariant.span);

    let main = program.exports["main"];
    assert_eq!(
        Interpreter::new(&program)
            .invoke(main, Vec::new(), Span::default())
            .expect("interpreter validates structured ConstraintError"),
        Value::Unit
    );

    let directory = tempfile::tempdir().expect("create runtime constraint output");
    let executable = directory.path().join("runtime-constraint");
    let ir = directory.path().join("runtime-constraint.ll");
    let mut options = EmitOptions::run("main").with_optimization(OptimizationProfile::Release);
    options.emit_ir = Some(ir.clone());
    assert_eq!(
        emit_automatic_executable(&program, &executable, options),
        NativeRouteKind::Lcir
    );
    let llvm = std::fs::read_to_string(ir).expect("read runtime constraint LLVM IR");
    assert!(!llvm.contains("loom.Value"), "{llvm}");
    let output = Command::new(executable)
        .output()
        .expect("run typed runtime constraint");
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn decoded_refinement_proof_rechecks_before_interpreter_or_typed_execution() {
    let source = r"module proof_provenance

type Positive = Float where self >= 0.0

pub fn main() Unit {
    discard Positive(10.0)
    Unit
}
";
    let fresh = compile_source(source);
    let fresh_debug = format!("{fresh:#?}");
    assert!(
        fresh_debug.contains("construction: Proven"),
        "{fresh_debug}"
    );
    assert_fresh_proof_uses_lcir(&fresh, "main");

    let directory = tempfile::tempdir().expect("create proof output directory");
    let fresh_executable = directory.path().join("fresh");
    let fresh_ir = directory.path().join("fresh.ll");
    let mut fresh_options =
        EmitOptions::run("main").with_optimization(OptimizationProfile::Release);
    fresh_options.emit_ir = Some(fresh_ir.clone());
    emit_native(&fresh, &fresh_executable, &fresh_options).expect("emit fresh proven source");
    let fresh_llvm = std::fs::read_to_string(fresh_ir).expect("read fresh proof LLVM IR");
    assert!(!fresh_llvm.contains(ARTIFACT_PROOF_REJECTED_FAULT_CODE));
    assert!(!fresh_llvm.contains("artifact.proof.recheck"));
    assert!(
        Command::new(&fresh_executable)
            .output()
            .expect("run fresh proven source")
            .status
            .success()
    );

    let (decoded, entry) = decode_with_tampered_float(&fresh, "main", 10.0, -1.0, true);
    let decoded_debug = format!("{decoded:#?}");
    assert!(
        decoded_debug.contains("construction: Recheck"),
        "{decoded_debug}"
    );
    assert!(
        !decoded_debug.contains("construction: Proven"),
        "{decoded_debug}"
    );
    assert!(decoded.serialized_construction_proofs_were_distrusted());

    let function = decoded.exports[&entry];
    let expected_span = first_evaluated_expression_span(&decoded, function);
    let failure = Interpreter::new(&decoded)
        .invoke(function, Vec::new(), Span::default())
        .expect_err("forged refinement must not enter the interpreter as a nominal value");
    assert_canonical_proof_failure(&failure, expected_span);

    let prepared = prepare_native_object(
        &decoded,
        EmitOptions::run(&entry).with_optimization(OptimizationProfile::Release),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare decoded proof artifact");
    assert_eq!(
        prepared.route_kind(),
        NativeRouteKind::Lcir,
        "nongeneric proof replay must use an explicit typed LCIR fault guard"
    );

    let decoded_ir = directory.path().join("decoded.ll");
    let decoded_executable = directory.path().join("decoded");
    let mut decoded_options =
        EmitOptions::run(&entry).with_optimization(OptimizationProfile::Release);
    decoded_options.emit_ir = Some(decoded_ir.clone());
    assert_eq!(
        emit_automatic_executable(&decoded, &decoded_executable, decoded_options),
        NativeRouteKind::Lcir
    );
    let decoded_llvm = std::fs::read_to_string(decoded_ir).expect("read decoded proof LLVM IR");
    assert!(
        decoded_llvm.contains(ARTIFACT_PROOF_REJECTED_FAULT_CODE),
        "{decoded_llvm}"
    );
    assert!(!decoded_llvm.contains("loom.Value"), "{decoded_llvm}");
    let output = Command::new(decoded_executable)
        .env(FAULT_FORMAT_ENV, FAULT_FORMAT_JSON)
        .output()
        .expect("run decoded proof recheck");
    assert!(!output.status.success(), "{output:?}");
    let native_failure = String::from_utf8(output.stderr)
        .expect("native proof stderr is UTF-8")
        .lines()
        .find_map(|line| line.strip_prefix(FAULT_JSON_PREFIX))
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("native fault JSON"))
        .expect("native proof failure has a structured record");
    assert_eq!(
        native_failure,
        serde_json::to_value(&failure).expect("serialize interpreted proof failure")
    );

    assert_eq!(
        Interpreter::new(&fresh)
            .invoke(fresh.exports["main"], Vec::new(), Span::default())
            .expect("fresh proven source remains directly executable"),
        Value::Unit
    );
}

#[test]
fn decoded_record_proof_uses_the_same_canonical_fault_and_span() {
    let source = r"module record_proof_provenance

record NonNegative {
    value Float

    invariant self.value >= 0.0
}

pub fn main() Unit {
    discard NonNegative { value = 11.0 }
    Unit
}
";
    let fresh = compile_source(source);
    let (decoded, entry) = decode_with_tampered_float(&fresh, "main", 11.0, -1.0, true);
    let function = decoded.exports[&entry];
    let expected_span = first_evaluated_expression_span(&decoded, function);
    let failure = Interpreter::new(&decoded)
        .invoke(function, Vec::new(), Span::default())
        .expect_err("forged record must not become an established nominal value");
    assert_canonical_proof_failure(&failure, expected_span);

    let directory = tempfile::tempdir().expect("create record proof output");
    let prepared = prepare_native_object(
        &decoded,
        EmitOptions::run(&entry).with_optimization(OptimizationProfile::Development),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare record proof replay");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    let debug_executable = directory.path().join("record-proof-debug");
    let debug_ir = directory.path().join("record-proof-debug.ll");
    let mut debug_options =
        EmitOptions::run(&entry).with_optimization(OptimizationProfile::Development);
    debug_options.emit_ir = Some(debug_ir.clone());
    assert_eq!(
        emit_automatic_executable(&decoded, &debug_executable, debug_options),
        NativeRouteKind::Lcir
    );
    let llvm = std::fs::read_to_string(debug_ir).expect("read record proof replay LLVM IR");
    assert!(llvm.contains(ARTIFACT_PROOF_REJECTED_FAULT_CODE), "{llvm}");
    assert!(!llvm.contains("loom.Value"), "{llvm}");
    assert_eq!(
        native_json_failure(&decoded, &entry, directory.path(), "record-proof"),
        serde_json::to_value(&failure).expect("serialize interpreted record proof failure")
    );
}

#[test]
fn decoded_generic_record_recheck_preserves_instantiated_contract_types() {
    let source = r"module generic_proof_provenance

record Boxed[T] {
    payload T
    marker Int

    invariant self.marker >= 0
}

pub fn main() Unit {
    discard Boxed { payload = 7, marker = 9 }
    Unit
}
";
    let fresh = compile_source(source);
    let fresh_debug = format!("{fresh:#?}");
    assert!(
        fresh_debug.contains("construction: Proven"),
        "{fresh_debug}"
    );

    let bytes = encode_interpreted_executable_artifact(&fresh, "main")
        .expect("encode generic proof artifact");
    let (decoded, entry) =
        decode_interpreted_executable_artifact(&bytes).expect("decode generic proof artifact");
    let decoded_debug = format!("{decoded:#?}");
    assert!(
        decoded_debug.contains("construction: Recheck"),
        "{decoded_debug}"
    );
    assert_eq!(
        Interpreter::new(&decoded)
            .invoke(decoded.exports[&entry], Vec::new(), Span::default())
            .expect("generic invariant replay succeeds in the interpreter"),
        Value::Unit
    );

    let prepared = prepare_native_object(
        &decoded,
        EmitOptions::run(&entry).with_optimization(OptimizationProfile::Release),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare generic invariant recheck");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Legacy);

    let directory = tempfile::tempdir().expect("create generic proof output");
    let executable = directory.path().join("generic-proof");
    let ir = directory.path().join("generic-proof.ll");
    let mut options = EmitOptions::run(&entry).with_optimization(OptimizationProfile::Release);
    options.emit_ir = Some(ir.clone());
    emit_native(&decoded, &executable, &options).expect("emit generic invariant recheck");
    let llvm = std::fs::read_to_string(ir).expect("read generic proof LLVM IR");
    assert!(llvm.contains("artifact.proof.recheck"), "{llvm}");
    let output = Command::new(executable)
        .output()
        .expect("run generic invariant recheck");
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn proof_bearing_disk_cache_reanalysis_preserves_native_route_and_ir() {
    let source = r"module proof_cache_parity

type Positive = Float where self >= 0.0

pub fn main() Unit {
    discard Positive(10.0)
    Unit
}
";
    let project = tempfile::tempdir().expect("create proof cache parity project");
    std::fs::write(project.path().join("main.loom"), source).expect("write proof cache source");
    let cache = PersistentCache::new(project.path().join("compiler-cache"));

    let cold_host = AnalysisHost::new(project.path()).expect("load cold proof project");
    let cold_sources = cold_host.load_sources().expect("load cold proof sources");
    let (cold, cold_parse) = cold_host.snapshot_from_sources_with_parse_cache(
        cold_sources,
        &cache,
        "proof-cache-parity-v1",
    );
    assert!(!cold.has_errors(), "{:#?}", cold.diagnostics());
    assert_eq!(cold_parse.misses, 1);
    let cold_program = cold.executable().expect("lower cold proof MIR");

    let warm_host = AnalysisHost::new(project.path()).expect("load warm proof project");
    let warm_sources = warm_host.load_sources().expect("load warm proof sources");
    let (warm, warm_parse) = warm_host.snapshot_from_sources_with_parse_cache(
        warm_sources,
        &cache,
        "proof-cache-parity-v1",
    );
    assert!(!warm.has_errors(), "{:#?}", warm.diagnostics());
    assert!(warm_parse.is_full_hit());
    assert_eq!(warm.semantic_query_stats().modules_reused, 0);
    let warm_program = warm.executable().expect("lower rebuilt warm proof MIR");
    assert_eq!(format!("{cold_program:#?}"), format!("{warm_program:#?}"));

    let output = tempfile::tempdir().expect("create proof cache parity output");
    let cold_ir = output.path().join("cold.ll");
    let warm_ir = output.path().join("warm.ll");
    let mut cold_options = EmitOptions::run("main").with_optimization(OptimizationProfile::Release);
    cold_options.emit_ir = Some(cold_ir.clone());
    let mut warm_options = EmitOptions::run("main").with_optimization(OptimizationProfile::Release);
    warm_options.emit_ir = Some(warm_ir.clone());
    let cold_prepared =
        prepare_native_object(cold_program, cold_options, NativeRoutePolicy::Automatic)
            .expect("prepare cold proof object");
    let warm_prepared =
        prepare_native_object(warm_program, warm_options, NativeRoutePolicy::Automatic)
            .expect("prepare rebuilt warm proof object");
    assert_eq!(cold_prepared.route_kind(), warm_prepared.route_kind());
    assert_eq!(
        cold_prepared.route_kind(),
        NativeRouteKind::Lcir,
        "disk-cache reanalysis must recover the fresh Proven LCIR route"
    );
    emit_prepared_native_object(&cold_prepared, &output.path().join("cold.o"))
        .expect("emit cold proof object");
    emit_prepared_native_object(&warm_prepared, &output.path().join("warm.o"))
        .expect("emit warm proof object");
    let cold_llvm = std::fs::read_to_string(cold_ir).expect("read cold proof IR");
    let warm_llvm = std::fs::read_to_string(warm_ir).expect("read warm proof IR");
    assert_eq!(cold_llvm, warm_llvm);
    assert!(!cold_llvm.contains(ARTIFACT_PROOF_REJECTED_FAULT_CODE));
}

#[test]
fn decoded_proof_faults_follow_normal_settled_and_race_task_containment() {
    let source = r#"module async_proof_provenance

type Positive = Float where self >= 0.0

async fn invalid() Positive {
    Positive(10.0)
}

pub async fn main() Unit {
    let first, second = Task.settled(invalid(), invalid()).await
    match first {
        Completed(_) => {
            assert false
            Unit
        }
        Faulted(fault) => {
            let code = fault.code()
            let message = fault.message()
            assert code == "__PROOF_CODE__"
            assert message == "__PROOF_MESSAGE__"
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
    match second {
        Completed(_) => {
            assert false
            Unit
        }
        Faulted(fault) => {
            let code = fault.code()
            let message = fault.message()
            assert code == "__PROOF_CODE__"
            assert message == "__PROOF_MESSAGE__"
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
    match Task.race(invalid(), invalid()).await {
        Completed(_) => {
            assert false
            Unit
        }
        Faulted(fault) => {
            let code = fault.code()
            let message = fault.message()
            assert code == "__PROOF_CODE__"
            assert message == "__PROOF_MESSAGE__"
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
    Unit
}
"#
    .replace("__PROOF_CODE__", ARTIFACT_PROOF_REJECTED_FAULT_CODE)
    .replace("__PROOF_MESSAGE__", ARTIFACT_PROOF_REJECTED_FAULT_MESSAGE);
    let fresh = compile_source(&source);
    let (decoded, entry) = decode_with_tampered_float(&fresh, "main", 10.0, -1.0, false);
    assert_eq!(
        Interpreter::new(&decoded)
            .invoke(decoded.exports[&entry], Vec::new(), Span::default())
            .expect("Task.settled/race contain proof replay faults"),
        Value::Unit
    );

    let directory = tempfile::tempdir().expect("create async proof output");
    let executable = directory.path().join("async-proof");
    let options = EmitOptions::run(&entry).with_optimization(OptimizationProfile::Release);
    emit_native(&decoded, &executable, &options).expect("emit async proof containment");
    let output = Command::new(executable)
        .output()
        .expect("run async proof containment");
    assert!(output.status.success(), "{output:?}");
}
