#![allow(clippy::default_trait_access)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::process::{Command, Output, Stdio};

use loom_codegen_ir::{
    CheckedArtifact, Effects, InstanceId, InstanceRole, InstanceWitnessArgument, InstructionKind,
    LoweringOutcome, Repr, ResourceKind, SourceArtifactRequest, TargetLayout, UnsupportedFeature,
    dump_program, lower_typed_artifact,
};
use loom_codegen_llvm::{
    DebugSource, EmitOptions, NativeObjectOptions, NativeRouteKind, NativeRoutePolicy,
    OptimizationProfile, emit_lcir_native_object, emit_prepared_native_object,
    prepare_native_object,
};
use loom_core::runtime_fault::{
    EMPTY_TASK_JOIN_FAULT_CODE, EMPTY_TASK_JOIN_FAULT_MESSAGE, INTEGER_OVERFLOW_FAULT_CODE,
    INTEGER_OVERFLOW_FAULT_MESSAGE, INVALID_SLEEP_DURATION_FAULT_CODE,
    INVALID_SLEEP_DURATION_FAULT_MESSAGE, SLEEP_DURATION_OVERFLOW_FAULT_CODE,
    SLEEP_DURATION_OVERFLOW_FAULT_MESSAGE, TASK_ANY_FAILED_FAULT_CODE,
    TASK_ANY_FAILED_FAULT_MESSAGE,
};
use loom_interpreter::{ExecutionFailure, Interpreter, TestStatus, Value};
use loom_mir::{
    CheckedProgram, Constant as MirConstant, ContractExprKind, ExprKind, Function, Pattern,
    StatementKind, Type, UnaryOp,
};
use loom_runtime_abi::{
    FAULT_FORMAT_ENV, FAULT_FORMAT_JSON, FAULT_JSON_PREFIX, FORMAT_FLOAT_TYPED_SYMBOL,
    PARSE_FLOAT_SYMBOL, TYPED_JSON_FORMAT_SYMBOL,
};

mod support;
#[cfg(unix)]
use support::run_with_closed_stderr;
use support::{emit_native, link_native_object, run_with_read_only_stderr};

const TYPED_LOGGING_INTERPRETER_CHILD_ENV: &str = "LOOM_LCIR_TYPED_LOGGING_INTERPRETER_CHILD";
const TYPED_STDOUT_INTERPRETER_CHILD_ENV: &str = "LOOM_LCIR_TYPED_STDOUT_INTERPRETER_CHILD";
const FINITE_DYN_FAULT_INTERPRETER_CHILD_ENV: &str = "LOOM_LCIR_FINITE_DYN_FAULT_INTERPRETER_CHILD";
const TYPED_LOGGING_STDERR: &[u8] =
    include_bytes!("../../../fixtures/lcir-typed-logging/expected.stderr");

struct NativeRun {
    ir: String,
    output: Output,
}

fn assert_dump_has_nominal(dump: &str, id: u32) {
    let expected = format!("Nominal#{id}[] =>");
    assert!(dump.contains(&expected), "missing `{expected}`:\n{dump}");
}

fn assert_interpreted_tests_pass(program: &CheckedProgram) {
    let interpreted = Interpreter::new(program).run_tests();
    assert!(
        interpreted
            .iter()
            .all(|test| test.status == TestStatus::Passed),
        "{interpreted:?}"
    );
}

fn analyze_source(source: &str) -> loom_driver::AnalysisSnapshot {
    analyze_project_sources(source, None)
}

fn analyze_sources(source: &str, test_source: &str) -> loom_driver::AnalysisSnapshot {
    analyze_project_sources(source, Some(test_source))
}

fn analyze_project_sources(
    source: &str,
    test_source: Option<&str>,
) -> loom_driver::AnalysisSnapshot {
    let project = tempfile::tempdir().expect("create source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source fixture");
    if let Some(test_source) = test_source {
        std::fs::write(project.path().join("main_test.loom"), test_source)
            .expect("write test source fixture");
    }
    let snapshot = support::analysis_host(project.path())
        .expect("load source project")
        .snapshot()
        .expect("analyze source project");
    assert!(
        !snapshot.has_errors(),
        "source diagnostics: {:#?}",
        snapshot.diagnostics()
    );
    snapshot
}

fn snapshot_debug_sources(snapshot: &loom_driver::AnalysisSnapshot) -> Vec<DebugSource> {
    snapshot
        .sources()
        .documents()
        .iter()
        .map(|document| {
            DebugSource::new(
                document.id().0,
                document.relative_path(),
                document.text().expect("checked source must be valid UTF-8"),
            )
        })
        .collect()
}

fn compile_source(source: &str) -> CheckedProgram {
    analyze_source(source)
        .executable()
        .expect("lower checked MIR")
        .clone()
}

fn compile_sources(source: &str, test_source: &str) -> CheckedProgram {
    analyze_sources(source, test_source)
        .executable()
        .expect("lower checked MIR")
        .clone()
}

fn compile_source_with_debug_sources(source: &str) -> (CheckedProgram, Vec<DebugSource>) {
    let snapshot = analyze_source(source);
    let debug_sources = snapshot_debug_sources(&snapshot);
    let program = snapshot.executable().expect("lower checked MIR").clone();
    (program, debug_sources)
}

fn compile_sources_with_debug_sources(
    source: &str,
    test_source: &str,
) -> (CheckedProgram, Vec<DebugSource>) {
    let snapshot = analyze_sources(source, test_source);
    let debug_sources = snapshot_debug_sources(&snapshot);
    let program = snapshot.executable().expect("lower checked MIR").clone();
    (program, debug_sources)
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

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one source gate keeps normal, child-fault, and sibling-cancellation cleanup plus exact callback descriptors together"
)]
fn typed_async_cleanup_crosses_suspension_without_a_runtime_cleanup_stack() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-async-cleanup/main.loom"),
        include_str!("../../../fixtures/lcir-async-cleanup/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    for required in [
        "await_tasks all state",
        " fault b",
        " cancel b",
        "task.cancelled",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }

    let normal = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("normalCleanup"))
        .expect("defer-bearing coroutine");
    let scoped = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("scopedCleanup"))
        .expect("scoped-disposal coroutine");
    for function in [normal, scoped] {
        assert!(
            function.effects().contains(Effects::MAY_FAULT)
                && function.effects().contains(Effects::MAY_SUSPEND),
            "await plus cleanup must carry exact fault and suspension effects: {}",
            function.name()
        );
        assert_eq!(
            function
                .coroutine()
                .expect("checked coroutine plan")
                .suspensions()
                .len(),
            1,
            "fixture keeps one exact cleanup-bearing suspension row"
        );
    }

    let lcir = emit_and_run_lcir_machine_fault(&artifact, "lcir-async-cleanup-normal");
    let checked_mir = emit_and_run_checked_mir_machine_fault(
        &program,
        "main",
        "checked-mir-async-cleanup-normal",
    );
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
    assert_eq!(lcir.output.stderr, checked_mir.stderr);
    assert!(!lcir.ir.contains("loom.Value"), "{}", lcir.ir);
    for required in [
        "loom_typed_task_is_cancel_requested_v1",
        "coroutine.cancel.dispatch",
        "coroutine.cancel.live",
        "task.await.fault.live",
    ] {
        assert!(
            lcir.ir.contains(required),
            "normal cleanup IR omitted `{required}`:\n{}",
            lcir.ir
        );
    }
    for function in [normal, scoped] {
        let callback_name = format!("@loom.lcir.coroutine.resume.{}", function.id().raw());
        let descriptor_name = format!("@loom.lcir.coroutine.descriptor.{} =", function.id().raw());
        let descriptor = lcir
            .ir
            .lines()
            .find(|line| line.starts_with(&descriptor_name))
            .unwrap_or_else(|| panic!("missing descriptor `{descriptor_name}`"));
        assert_eq!(
            descriptor.matches(&callback_name).count(),
            2,
            "resume and cancellation must share the checked source callback: {descriptor}"
        );
    }

    let expected_fault_cleanup = serde_json::to_value(
        interpret_run(&program, "faultCleanupMain")
            .expect_err("the awaited child must establish the primary fault"),
    )
    .expect("serialize child-fault cleanup fixture fault");
    let fault_cleanup = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "faultCleanupMain".into(),
        },
    );
    let faulted = emit_and_run_lcir_machine_fault(&fault_cleanup, "lcir-async-cleanup-child-fault");
    let checked_mir_faulted = emit_and_run_checked_mir_machine_fault(
        &program,
        "faultCleanupMain",
        "checked-mir-async-cleanup-child-fault",
    );
    assert!(!faulted.output.status.success(), "{:?}", faulted.output);
    assert!(
        !checked_mir_faulted.status.success(),
        "{checked_mir_faulted:?}"
    );
    assert_eq!(machine_fault(&faulted.output), expected_fault_cleanup);
    assert_eq!(machine_fault(&checked_mir_faulted), expected_fault_cleanup);
    assert!(
        faulted.ir.contains("task.await.fault.live")
            && faulted.ir.contains("task.await.fault.active.pointer"),
        "awaited child fault omitted source-fault activation and cleanup continuation:\n{}",
        faulted.ir
    );

    let expected = serde_json::to_value(
        interpret_run(&program, "cancellationMain")
            .expect_err("the first Task.all child must fault"),
    )
    .expect("serialize cancellation fixture fault");
    let cancellation = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "cancellationMain".into(),
        },
    );
    let cancelled =
        emit_and_run_lcir_machine_fault(&cancellation, "lcir-async-cleanup-cancellation");
    let checked_mir_cancelled = emit_and_run_checked_mir_machine_fault(
        &program,
        "cancellationMain",
        "checked-mir-async-cleanup-cancellation",
    );
    assert!(!cancelled.output.status.success(), "{:?}", cancelled.output);
    assert!(
        !checked_mir_cancelled.status.success(),
        "{checked_mir_cancelled:?}"
    );
    assert_eq!(machine_fault(&cancelled.output), expected);
    assert_eq!(machine_fault(&checked_mir_cancelled), expected);
    assert!(
        !diagnostic_text(&cancelled.output).contains("LOOM_RUNTIME_TYPED_"),
        "source cleanup violated the typed cancellation protocol: {:?}",
        cancelled.output
    );
    let waiting = cancellation
        .functions()
        .iter()
        .find(|function| function.name().ends_with("waitsWithCleanup"))
        .expect("cancelled cleanup-bearing sibling");
    let callback_name = format!("@loom.lcir.coroutine.resume.{}", waiting.id().raw());
    let waiting_callback = cancelled
        .ir
        .split("\ndefine ")
        .find(|function| function.contains(&format!("{callback_name}(")))
        .expect("cancelled cleanup-bearing callback");
    assert!(
        waiting_callback.contains("coroutine.cancel.live")
            && waiting_callback.contains("ret i32 3"),
        "cancelled sibling omitted its static cleanup exit:\n{waiting_callback}"
    );
    let descriptor_name = format!("@loom.lcir.coroutine.descriptor.{} =", waiting.id().raw());
    let descriptor = cancelled
        .ir
        .lines()
        .find(|line| line.starts_with(&descriptor_name))
        .expect("cancelled sibling descriptor");
    assert_eq!(
        descriptor.matches(&callback_name).count(),
        2,
        "{descriptor}"
    );

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create async cleanup target output");
        let object = directory.path().join(if target.contains("windows") {
            "async-cleanup.obj"
        } else {
            "async-cleanup.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit async cleanup object for {target}: {error}"));
        assert!(
            object.is_file(),
            "missing async cleanup object for {target}"
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one source gate keeps managed normal, fault, and cancellation writeback together"
)]
fn typed_async_callers_reuse_synchronous_functional_writeback() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-async-writeback/main.loom"),
        include_str!("../../../fixtures/lcir-async-writeback/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("writebacks("), "{dump}");
    let verify = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("verifyWriteback"))
        .expect("writeback-bearing coroutine");
    assert!(
        verify.coroutine().is_some(),
        "the writeback caller must remain a checked coroutine"
    );
    assert!(
        verify.effects().contains(Effects::MAY_COLLECT),
        "managed receiver writeback must preserve moving-GC requirements"
    );

    let lcir = emit_and_run_lcir_machine_fault(&artifact, "lcir-async-writeback-normal");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert!(lcir.output.stderr.is_empty(), "{:?}", lcir.output);
    assert!(!lcir.ir.contains("loom.Value"), "{}", lcir.ir);
    assert_no_indirect_calls(&lcir.ir);

    let expected_fault = serde_json::to_value(
        interpret_run(&program, "faultWritebackMain")
            .expect_err("the synchronous callee must fault after mutating its receiver"),
    )
    .expect("serialize async writeback fault");
    let fault_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "faultWritebackMain".into(),
        },
    );
    let fault_dump = dump_program(fault_artifact.program());
    assert!(
        fault_dump.contains("resume_fault writebacks"),
        "fault-edge receiver writeback must precede coroutine cleanup:\n{fault_dump}"
    );
    let faulted = emit_and_run_lcir_machine_fault(&fault_artifact, "lcir-async-writeback-fault");
    assert!(!faulted.output.status.success(), "{:?}", faulted.output);
    let expected_code = expected_fault["fault"]["code"]
        .as_str()
        .expect("interpreter fault code");
    let lcir_fault = machine_fault(&faulted.output);
    assert_eq!(lcir_fault["code"].as_str(), Some(expected_code));

    let expected_cancellation = serde_json::to_value(
        interpret_run(&program, "cancellationAfterWritebackMain")
            .expect_err("the failing sibling must cancel the writeback-bearing child"),
    )
    .expect("serialize async writeback cancellation fault");
    let cancellation_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "cancellationAfterWritebackMain".into(),
        },
    );
    let append = cancellation_artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("append"))
        .expect("managed writeback callee");
    let waiting = cancellation_artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("waitsAfterWriteback"))
        .expect("writeback-bearing cancelled sibling");
    assert!(
        waiting.instructions().iter().any(|instruction| matches!(
            instruction.kind(),
            InstructionKind::DirectCall { callee, .. }
                if callee == &append.id() && instruction.results().len() == 2
        )),
        "cancelled coroutine omitted the exact receiver writeback"
    );
    let waiting_plan = waiting
        .coroutine()
        .expect("writeback-bearing sibling remains a coroutine");
    assert_eq!(waiting_plan.suspensions().len(), 1);
    assert!(
        waiting_plan.suspensions()[0]
            .live()
            .iter()
            .any(|ty| matches!(
                cancellation_artifact
                    .representations()
                    .value_type(*ty)
                    .and_then(|value| cancellation_artifact.representations().repr(value.repr())),
                Some(Repr::Product(_))
            )),
        "the cancellation row must retain the updated managed record"
    );
    let cancelled = emit_and_run_lcir_machine_fault(
        &cancellation_artifact,
        "lcir-async-writeback-cancellation",
    );
    assert!(!cancelled.output.status.success(), "{:?}", cancelled.output);
    assert_eq!(machine_fault(&cancelled.output), expected_cancellation);
    let callback_name = format!("@loom.lcir.coroutine.resume.{}", waiting.id().raw());
    let waiting_callback = cancelled
        .ir
        .split("\ndefine ")
        .find(|function| function.contains(&format!("{callback_name}(")))
        .expect("writeback-bearing cancellation callback");
    assert!(
        waiting_callback.contains("coroutine.cancel.live")
            && !diagnostic_text(&cancelled.output).contains("LOOM_RUNTIME_TYPED_"),
        "managed writeback violated cancellation cleanup: {:?}",
        cancelled.output
    );

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create async writeback target output");
        let object = directory.path().join(if target.contains("windows") {
            "async-writeback.obj"
        } else {
            "async-writeback.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit async writeback object for {target}: {error}"));
        assert!(
            object.is_file(),
            "missing async writeback object for {target}"
        );
    }
}

const PROJECTED_PLACE_SOURCE: &str =
    include_str!("../../../fixtures/lcir-projected-places/main.loom");
const PROJECTED_PLACE_TEST_SOURCE: &str =
    include_str!("../../../fixtures/lcir-projected-places/main_test.loom");

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

fn emit_and_run_checked_mir(program: &CheckedProgram, entry: &str, stem: &str) -> Output {
    emit_and_run_checked_mir_with_fault_format(program, entry, stem, false)
}

fn prepare_and_run_checked_mir(
    program: &CheckedProgram,
    options: EmitOptions,
    stem: &str,
) -> Output {
    let directory = tempfile::tempdir().expect("create checked-MIR output directory");
    let object = directory.path().join(format!("{stem}.o"));
    let executable = directory.path().join(stem);
    let prepared = prepare_native_object(program, options, NativeRoutePolicy::CheckedMirOnly)
        .expect("prepare checked-MIR native object");
    assert_eq!(prepared.route_kind(), NativeRouteKind::CheckedMir);
    emit_prepared_native_object(&prepared, &object).expect("emit checked-MIR native object");
    link_native_object(&object, &executable).expect("link checked-MIR native object");
    Command::new(executable)
        .output()
        .expect("run checked-MIR executable")
}

fn prepare_and_run_checked_mir_with_ir(
    program: &CheckedProgram,
    mut options: EmitOptions,
    stem: &str,
) -> NativeRun {
    let directory = tempfile::tempdir().expect("create checked-MIR output directory");
    let object = directory.path().join(format!("{stem}.o"));
    let ir_path = directory.path().join(format!("{stem}.ll"));
    let executable = directory.path().join(stem);
    options.emit_ir = Some(ir_path.clone());
    let prepared = prepare_native_object(program, options, NativeRoutePolicy::CheckedMirOnly)
        .expect("prepare checked-MIR native object");
    assert_eq!(prepared.route_kind(), NativeRouteKind::CheckedMir);
    emit_prepared_native_object(&prepared, &object).expect("emit checked-MIR native object");
    link_native_object(&object, &executable).expect("link checked-MIR native object");
    let output = Command::new(executable)
        .output()
        .expect("run checked-MIR executable");
    NativeRun {
        ir: std::fs::read_to_string(ir_path).expect("read checked-MIR LLVM IR"),
        output,
    }
}

fn emit_and_run_checked_mir_machine_fault(
    program: &CheckedProgram,
    entry: &str,
    stem: &str,
) -> Output {
    emit_and_run_checked_mir_with_fault_format(program, entry, stem, true)
}

fn emit_and_run_checked_mir_machine_fault_with_ir(
    program: &CheckedProgram,
    entry: &str,
    stem: &str,
) -> NativeRun {
    let directory = tempfile::tempdir().expect("create checked-MIR output directory");
    let executable = directory.path().join(stem);
    let ir_path = directory.path().join(format!("{stem}.ll"));
    let mut options = EmitOptions::run(entry);
    options.emit_ir = Some(ir_path.clone());
    emit_native(program, &executable, &options).expect("emit checked-MIR comparison executable");
    let output = Command::new(executable)
        .env(FAULT_FORMAT_ENV, FAULT_FORMAT_JSON)
        .output()
        .expect("run checked-MIR comparison executable");
    NativeRun {
        ir: std::fs::read_to_string(ir_path).expect("read checked-MIR LLVM IR"),
        output,
    }
}

fn emit_and_run_checked_mir_with_fault_format(
    program: &CheckedProgram,
    entry: &str,
    stem: &str,
    machine_faults: bool,
) -> Output {
    let directory = tempfile::tempdir().expect("create checked-MIR output directory");
    let executable = directory.path().join(stem);
    emit_native(program, &executable, &EmitOptions::run(entry))
        .expect("emit checked-MIR comparison executable");
    let mut command = Command::new(executable);
    if machine_faults {
        command.env(FAULT_FORMAT_ENV, FAULT_FORMAT_JSON);
    }
    command
        .output()
        .expect("run checked-MIR comparison executable")
}

fn emit_and_run_checked_mir_tests(program: &CheckedProgram, stem: &str) -> Output {
    let directory = tempfile::tempdir().expect("create checked-MIR test output directory");
    let executable = directory.path().join(stem);
    emit_native(program, &executable, &EmitOptions::tests())
        .expect("emit checked-MIR comparison test executable");
    Command::new(executable)
        .output()
        .expect("run checked-MIR comparison test executable")
}

#[test]
fn fallible_debug_metadata_describes_the_physical_abi_and_visible_parameters() {
    let source = include_str!("../../../fixtures/lcir-debug-fallible/main.loom");
    let (program, debug_sources) = compile_source_with_debug_sources(source);
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
        debug_sources,
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
    let source = r"record Counter {
    value Int
    enabled Bool
}

record Gauge {
    value Int
    enabled Bool
}

impl Counter {
    method reset(mut self, value Int) {
        self.value = value
    }

    method add(mut self, value Int) {
        self.value = self.value + value
    }
}

impl Gauge {
    method reset(mut self, value Int) {
        self.value = value
    }

    method add(mut self, value Int) {
        self.value = self.value + value
    }
}

fn forward(value Counter) Counter {
    value
}

pub fn main() {
    var counter = Counter { value = 1, enabled = true }
    counter.reset(2)
    counter.add(3)
    let copied = forward(counter)
    discard copied.value
    var gauge = Gauge { value = 4, enabled = false }
    gauge.reset(5)
    gauge.add(6)
    discard gauge.value
}
";
    let (program, debug_sources) = compile_source_with_debug_sources(source);
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
            debug_sources,
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
    let faults = machine_faults(output);
    assert_eq!(faults.len(), 1, "expected one machine fault: {output:?}");
    faults.into_iter().next().expect("one machine fault")
}

fn machine_faults(output: &Output) -> Vec<serde_json::Value> {
    let stderr = String::from_utf8(output.stderr.clone()).expect("machine fault is UTF-8");
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix(FAULT_JSON_PREFIX))
        .map(|json| serde_json::from_str(json).expect("machine fault is valid JSON"))
        .collect()
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
    emitted_lcir_instance(ir, function)
}

fn emitted_lcir_instance<'ir>(ir: &'ir str, function: &loom_codegen_ir::Function) -> &'ir str {
    let symbol = if function.coroutine().is_some() {
        format!("@loom.lcir.coroutine.resume.{}(", function.id().raw())
    } else {
        format!("@loom.lcir.fn.{}(", function.id().raw())
    };
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

fn emitted_checked_mir_function<'ir>(
    ir: &'ir str,
    program: &CheckedProgram,
    suffix: &str,
) -> &'ir str {
    let function = source_function(program, suffix);
    let symbol = format!("@loom.fn.{}.", function.id.0);
    let symbol_at = ir
        .find(&symbol)
        .unwrap_or_else(|| panic!("emitted checked-MIR function `{symbol}`"));
    let start = ir[..symbol_at]
        .rfind("\ndefine ")
        .map_or(0, |offset| offset + 1);
    let end = ir[symbol_at..]
        .find("\n}")
        .map_or(ir.len(), |offset| symbol_at + offset + 2);
    &ir[start..end]
}

fn assert_typed_resource_close_guard(
    ir: &str,
    artifact: &CheckedArtifact,
    kind: ResourceKind,
    resource_kind: u32,
) {
    let mut candidates = artifact.functions().iter().filter(|function| {
        function.instructions().iter().any(|instruction| {
            matches!(
                instruction.kind(),
                InstructionKind::ResourceClose { kind: actual, .. } if *actual == kind
            )
        })
    });
    let source = candidates
        .next()
        .unwrap_or_else(|| panic!("missing {kind:?} ResourceClose function"));
    assert!(
        candidates.next().is_none(),
        "multiple {kind:?} ResourceClose functions"
    );
    let function = emitted_lcir_instance(ir, source);
    for required in [
        "resource.close.status.ok = icmp eq i32",
        ", 0",
        "label %resource.close.ok",
        "label %resource.close.failed",
        "resource.close.failed:",
        "call void @llvm.trap()",
        "unreachable",
        "resource.close.token",
        "resource.close.writeback.token",
    ] {
        assert!(
            function.contains(required),
            "typed close status guard omitted `{required}`:\n{function}"
        );
    }
    let call = function
        .lines()
        .find(|line| line.contains("call i32 @loom_typed_resource_close_v1"))
        .expect("typed resource close call");
    assert!(
        call.contains(&format!("i32 {resource_kind}")),
        "typed resource close omitted kind {resource_kind}: {call}"
    );
    assert!(
        call.contains("ptr %__loom_executor"),
        "typed resource close did not receive the checked executor context: {call}"
    );
    assert!(
        function
            .lines()
            .filter(|line| line.contains("resource.close.status.ok = icmp eq i32"))
            .all(|line| line.trim_end().ends_with(", 0")),
        "only status zero may leave typed resource close:\n{function}"
    );
    for forbidden in [
        "switch i32 %resource.close.status",
        "resource.close.invalid_status",
        "resource.close.fault",
        "ResourceCloseFault",
        "resource.close.handle",
    ] {
        assert!(
            !function.contains(forbidden),
            "typed close retained `{forbidden}`:\n{function}"
        );
    }
}

fn checked_float_pattern_fixture() -> CheckedProgram {
    let source = r"fn classify(value Float) Int {
    match value {
        0.0 => 10
        1.0 => 20
        42.0 => 21
        _ => 30
    }
}

fn requireEqual(actual Int, expected Int) {
    if actual == expected {
        Unit
    } else {
        discard 1 / 0
        Unit
    }
}

pub fn main() {
    requireEqual(classify(0.0), 10)
    requireEqual(classify(-0.0), 10)
    requireEqual(classify(1.0), 20)
    requireEqual(classify(42.0), 30)
    requireEqual(classify(0.0 / 0.0), 30)
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

fn assert_typed_lcir_surface(ir: &str) {
    for forbidden in [
        "%loom.Value",
        "ArgNode",
        "ValueNode",
        "@loom.fn.",
        "loom_gc_root_push_v1",
        "loom_gc_root_pop_v1",
        "landingpad",
        "personality ptr",
        "resume {",
    ] {
        assert!(
            !ir.contains(forbidden),
            "universal value or EH token `{forbidden}` in typed LCIR:\n{ir}"
        );
    }
}

fn assert_stateless_direct_lcir_surface(ir: &str) {
    assert_typed_lcir_surface(ir);
    for forbidden in [
        "loom.runtime.print",
        "@puts",
        "@printf",
        "loom_executor_",
        "loom_gc_",
        "witness",
    ] {
        assert!(
            !ir.contains(forbidden),
            "stateful runtime token `{forbidden}` in direct LCIR:\n{ir}"
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
    assert_stateless_direct_lcir_surface(ir);
    assert!(ir.contains("loom_runtime_stdout_write_v1"), "{ir}");
    for line in ir.lines().filter(|line| line.contains("@loom_runtime_")) {
        assert!(
            line.contains("@loom_runtime_stdout_write_v1"),
            "pure LCIR has an unexpected runtime symbol: {line}\n{ir}"
        );
    }
    for forbidden in [
        "loom_runtime_create_v1",
        "loom_runtime_activate_v1",
        "loom_runtime_destroy_v1",
        "loom_runtime_deactivate_v1",
    ] {
        assert!(!ir.contains(forbidden), "unexpected `{forbidden}`:\n{ir}");
    }
    assert!(!ir.contains("loom_context_raise_fault_v1"), "{ir}");
}

fn assert_fallible_surface(ir: &str) {
    assert_stateless_direct_lcir_surface(ir);
    assert!(ir.contains("loom_runtime_create_v1"), "{ir}");
    assert!(ir.contains("loom_runtime_activate_v1"), "{ir}");
    assert!(ir.contains("loom_context_raise_fault_v1"), "{ir}");
    assert!(!ir.contains("loom_executor_"), "{ir}");
    assert!(!ir.contains("loom_gc_"), "{ir}");
}

const LIVE_SUM_CARRIER_SOURCE: &str = r"enum Packet {
    Empty
    Wide(Int)
    Bytes(Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool)
}

enum Problem { WrongCarrier }
";
const LIVE_SUM_CARRIER_TEST_SOURCE: &str = r"test fn carriesAcrossLoop() Result[Unit, Problem] {
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

const INTERLEAVED_MANAGED_SUM_RELEASE_SOURCE: &str = r"record PointerThenScalar { label Text, number Int }
record ScalarThenPointer { number Int, label Text }

enum Interleaved {
    PointerFirst(PointerThenScalar)
    PointerSecond(ScalarThenPointer)
}

enum Problem { WrongCarrier }

fn choose(flag Bool, label Text, number Int, depth Int) Interleaved {
    if depth > 0 {
        choose(flag, label, number, depth - 1)
    } else if flag {
        Interleaved.PointerFirst(PointerThenScalar { label = label, number = number })
    } else {
        Interleaved.PointerSecond(ScalarThenPointer { number = number, label = label })
    }
}

fn number(value Interleaved) Int {
    match value {
        PointerFirst(payload) => payload.number
        PointerSecond(payload) => payload.number
    }
}
";
const INTERLEAVED_MANAGED_SUM_RELEASE_TEST_SOURCE: &str = r#"test fn keepsManagedCarrierInRegisters() Result[Unit, Problem] {
    let label = "ma".concat("naged")
    var value = choose(true, label, 0, 1)
    var flag = true
    for index in 0..1000 {
        value = choose(flag, label, index, 1)
        flag = !flag
        Unit
    }
    if number(value) == 999 { Ok(Unit) } else { Err(Problem.WrongCarrier) }
}
"#;

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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-float-patterns");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
}

#[test]
fn executable_text_literal_patterns_match_managed_values_after_relocation() {
    let pressure = "x".repeat(40 * 1024);
    let source = r#"enum Envelope {
    Empty
    Wrapped(Option[Text])
}

fn join(left Text, right Text) Text { left.concat(right) }

fn collectPressure() Text { "__PRESSURE__".concat("__PRESSURE__") }

fn classify(value Text) Int {
    let pressure = collectPressure()
    discard pressure.length()
    match value {
        "hit" => 1
        "miss" => 2
        _ => 3
    }
}

fn classifyNested(value Envelope) Int {
    let pressure = collectPressure()
    discard pressure.length()
    match value {
        Wrapped(Some("nested")) => 4
        Wrapped(Some(_)) => 5
        _ => 6
    }
}

pub fn main() {
    let hit = classify(join("h", "it"))
    let miss = classify(join("m", "iss"))
    let fallback = classify(join("other", ""))
    let nestedHit = classifyNested(Envelope.Wrapped(Some(join("nest", "ed"))))
    let nestedFallback = classifyNested(Envelope.Wrapped(Some(join("other", ""))))
    let empty = classifyNested(Envelope.Empty)
    assert hit == 1
    assert miss == 2
    assert fallback == 3
    assert nestedHit == 4
    assert nestedFallback == 5
    assert empty == 6
}
"#
    .replace("__PRESSURE__", &pressure);
    let program = compile_source(&source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert_eq!(dump.matches("text.compare.equal").count(), 3, "{dump}");
    assert!(dump.contains("managed_ptr"), "{dump}");
    assert!(!dump.contains("loom.Value"), "{dump}");

    let native = emit_and_run_lcir(&artifact, "source-text-literal-patterns");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert!(native.output.stderr.is_empty(), "{:?}", native.output);
    assert!(
        native.ir.contains("text.compare.same_length"),
        "{}",
        native.ir
    );
    assert!(native.ir.contains("text.compare.equal"), "{}", native.ir);
    for suffix in ["classify", "classifyNested"] {
        let function = emitted_lcir_function(&native.ir, &artifact, suffix);
        for required in [
            "loom_gc_typed_root_push_v1",
            "managed.root.reload",
            "text.compare.equal",
        ] {
            assert!(
                function.contains(required),
                "{suffix} omitted `{required}`:\n{function}"
            );
        }
    }
    assert_typed_lcir_surface(&native.ir);
}

#[test]
fn source_lowered_pure_scalars_run_without_runtime_or_universal_values() {
    let source = r"fn choose(flag Bool, left Float, right Float) Bool {
    if flag { left < right } else { !flag }
}

pub fn main() {
    discard choose(true, 1.0, 2.0)
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
fn compile_time_constants_match_interpreter_typed_lcir_and_native_code() {
    let program = compile_source(
        r#"const base Int = 40
const answer Int = base + 2
const name Text = "loom"

fn matches(value Int, label Text) Bool {
    value == 42 && label == "loom"
}

pub fn main() {
    assert answer == 42 && name == "loom"
    discard matches(answer, name)
}
"#,
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("const int 42"), "{dump}");
    assert!(dump.contains("text.literal \"loom\""), "{dump}");

    let native = emit_and_run_lcir(&artifact, "source-compile-time-constants");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(!native.ir.contains("loom.Value"), "{}", native.ir);
}

#[test]
fn user_task_policy_method_names_stay_on_the_pure_native_call_path() {
    let source = r"record Scheduler { base Int }

impl Scheduler {
    method any(self, value Int) Int { value }
    method race(self, value Int) Int {
        discard value
        self.base
    }
}

pub fn main() {
    let Task = Scheduler { base = 10 }
    discard Task.any(2)
    discard Task.race(3)
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
            .all(|function| !function.effects().contains(Effects::NEEDS_EXECUTOR)),
        "ordinary user methods must not acquire executor effects: {:#?}",
        artifact.functions()
    );
    let dump = dump_program(artifact.program());
    for forbidden in ["await_tasks", "task.join", "task.sleep"] {
        assert!(
            !dump.contains(forbidden),
            "ordinary user methods lowered as `{forbidden}`:\n{dump}"
        );
    }

    let native = emit_and_run_lcir(&artifact, "source-user-task-methods");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert!(native.output.stderr.is_empty(), "{:?}", native.output);
    assert_pure_surface(&native.ir);
    for forbidden in ["loom_executor_", "loom_task_join_", "loom_typed_task_"] {
        assert!(
            !native.ir.contains(forbidden),
            "ordinary user methods emitted `{forbidden}`:\n{}",
            native.ir
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one gate covers the source integer parser, scalar ABI status, managed Text input, target objects, and universal-surface exclusion"
)]
fn scalar_builtin_apis_match_interpreter_and_close_typed_targets() {
    let source = r#"import std.float.FloatToIntError
import std.float.from_int
import std.float.is_finite
import std.float.parse_float
import std.float.to_int
import std.int.ParseIntError
import std.int.parse_int
import std.time.milliseconds

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
        Err(std.float.ParseFloatError.InvalidSyntax) => true
        _ => false
    }
}

fn floatOutOfRange(input Text) Bool {
    match parse_float(input) {
        Err(std.float.ParseFloatError.OutOfRange) => true
        _ => false
    }
}

pub fn main() {
    let managed = join("92", "23372036854775807")
    let maxInt = parsedIntEquals(managed, 9223372036854775807)
    let minInt = parsedIntEquals("-9223372036854775808", -9223372036854775807 - 1)
    let plusInt = parsedIntEquals("+17", 17)
    let zeroInt = parsedIntEquals("0", 0)
    let plusZeroInt = parsedIntEquals("+0", 0)
    let negativeZeroInt = parsedIntEquals("-0", 0)
    let leadingZeroInt = parsedIntEquals("00000017", 17)
    let invalidInt = intInvalid("17x")
    let emptyInt = intInvalid("")
    let plusOnlyInt = intInvalid("+")
    let minusOnlyInt = intInvalid("-")
    let whitespaceInt = intInvalid(" 17")
    let separatorInt = intInvalid("1_7")
    let radixInt = intInvalid("0x11")
    let duplicateSignInt = intInvalid("++17")
    let unicodeInt = intInvalid("１７")
    let overflowThenInvalidInt = intInvalid("999999999999999999999999999999999999x")
    let positiveIntOverflow = intOutOfRange("9223372036854775808")
    let negativeIntOverflow = intOutOfRange("-9223372036854775809")
    let longIntOverflow = intOutOfRange("999999999999999999999999999999999999")
    assert maxInt
    assert minInt
    assert plusInt
    assert zeroInt
    assert plusZeroInt
    assert negativeZeroInt
    assert leadingZeroInt
    assert invalidInt
    assert emptyInt
    assert plusOnlyInt
    assert minusOnlyInt
    assert whitespaceInt
    assert separatorInt
    assert radixInt
    assert duplicateSignInt
    assert unicodeInt
    assert overflowThenInvalidInt
    assert positiveIntOverflow
    assert negativeIntOverflow
    assert longIntOverflow

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

    let roundedInteger = from_int(9007199254740993)
    let truncatedPositive = match to_int(12.75) {
        Ok(value) => value == 12
        Err(_) => false
    }
    let truncatedNegative = match to_int(-12.75) {
        Ok(value) => value == -12
        Err(_) => false
    }
    let convertedMinimum = match to_int(-9223372036854775808.0) {
        Ok(value) => value == -9223372036854775807 - 1
        Err(_) => false
    }
    let nonFiniteConversion = match to_int(0.0 / 0.0) {
        Err(FloatToIntError.NonFinite) => true
        _ => false
    }
    let outOfRangeConversion = match to_int(9223372036854775808.0) {
        Err(FloatToIntError.OutOfRange) => true
        _ => false
    }
    assert roundedInteger == 9007199254740992.0
    assert truncatedPositive
    assert truncatedNegative
    assert convertedMinimum
    assert nonFiniteConversion
    assert outOfRangeConversion

    let delay = milliseconds(42)
    let observed = delay.as_milliseconds()
    assert observed == 42
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
        "parse.float.status",
        "convert.int_to_float",
        "convert.float_to_int_status",
        "text.encode_utf8",
        "bytes.length",
        "bytes.get",
        "float.compare.ordered_greater_equal",
        "float.compare.ordered_less_equal",
        "runtime InvalidDuration",
        "product.construct",
        "product.extract",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    assert!(
        !dump.contains(" = parse.int "),
        "integer parsing retained a dedicated LCIR instruction:\n{dump}"
    );
    let native = emit_and_run_lcir(&artifact, "source-scalar-builtins");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert!(native.output.stderr.is_empty(), "{:?}", native.output);
    for required in [
        PARSE_FLOAT_SYMBOL,
        "parse.float.status.valid",
        "parse.float.status.failed",
        "call void @llvm.trap()",
        "sitofp i64",
        "fptosi double",
    ] {
        assert!(
            native.ir.contains(required),
            "scalar builtin IR omitted `{required}`:\n{}",
            native.ir
        );
    }
    let failed = "parse.float.status.failed:";
    let start = native
        .ir
        .rfind(failed)
        .unwrap_or_else(|| panic!("missing unexpected-status block `{failed}`"));
    let end = native.ir.len().min(start + 512);
    assert!(
        native.ir[start..end].contains("call void @llvm.trap()"),
        "parse.float must trap an ABI-forged status outside 0/1/2:\n{}",
        &native.ir[start..end]
    );
    let parse_integer = emitted_lcir_function(&native.ir, &artifact, "parsedIntEquals");
    assert!(!parse_integer.contains("loom_gc_typed_root_push_v1"));
    assert!(!parse_integer.contains("loom_gc_typed_root_pop_v1"));
    assert!(
        !native.ir.contains("loom_runtime_parse_int"),
        "{}",
        native.ir
    );
    assert_stateless_direct_lcir_surface(&native.ir);

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
        assert!(!ir.contains("loom_runtime_parse_int"), "{ir}");
        assert!(ir.contains("sitofp i64"), "{ir}");
        assert!(ir.contains("fptosi double"), "{ir}");
        assert!(ir.contains(PARSE_FLOAT_SYMBOL), "{ir}");
        assert_stateless_direct_lcir_surface(&ir);
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate covers every conversion boundary and both native routes"
)]
fn explicit_numeric_conversions_are_pure_source_backed_typed_lcir() {
    let source = r"import std.float.FloatToIntError
import std.float.from_int
import std.float.to_int

fn rounded(value Int) Float {
    from_int(value)
}

fn truncated(value Float, expected Int) Bool {
    match to_int(value) {
        Ok(converted) => converted == expected
        Err(_) => false
    }
}

pub fn main() {
    let roundedTie = rounded(9007199254740993)
    let roundedMinimum = rounded(-9223372036854775807 - 1)
    let roundedMaximum = rounded(9223372036854775807)
    let truncatedPositive = truncated(12.75, 12)
    let truncatedNegative = truncated(-12.75, -12)
    let truncatedNegativeZero = truncated(-0.0, 0)
    let truncatedMaximum = truncated(9223372036854774784.0, 9223372036854774784)
    let minimum = match to_int(-9223372036854775808.0) {
        Ok(value) => value == -9223372036854775807 - 1
        Err(_) => false
    }
    let nonFinite = match to_int(0.0 / 0.0) {
        Err(FloatToIntError.NonFinite) => true
        _ => false
    }
    let positiveInfinity = match to_int(1.0 / 0.0) {
        Err(FloatToIntError.NonFinite) => true
        _ => false
    }
    let negativeInfinity = match to_int(-1.0 / 0.0) {
        Err(FloatToIntError.NonFinite) => true
        _ => false
    }
    let outOfRange = match to_int(9223372036854775808.0) {
        Err(FloatToIntError.OutOfRange) => true
        _ => false
    }
    let belowMinimum = match to_int(-9223372036854777856.0) {
        Err(FloatToIntError.OutOfRange) => true
        _ => false
    }
    assert roundedTie == 9007199254740992.0
    assert roundedMinimum == -9223372036854775808.0
    assert roundedMaximum == 9223372036854775808.0
    assert truncatedPositive
    assert truncatedNegative
    assert truncatedNegativeZero
    assert truncatedMaximum
    assert minimum
    assert nonFinite
    assert positiveInfinity
    assert negativeInfinity
    assert outOfRange
    assert belowMinimum
}
";
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));

    for policy in [NativeRoutePolicy::Automatic, NativeRoutePolicy::LcirOnly] {
        let prepared = prepare_native_object(&program, EmitOptions::run("main"), policy)
            .unwrap_or_else(|error| panic!("prepare numeric conversions with {policy:?}: {error}"));
        assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    }

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    for required in ["convert.int_to_float", "convert.float_to_int_status"] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    for wrapper in ["std.float.from_int", "std.float.to_int"] {
        let function = artifact
            .functions()
            .iter()
            .find(|function| function.name().ends_with(wrapper))
            .unwrap_or_else(|| panic!("missing source wrapper `{wrapper}`:\n{dump}"));
        assert_eq!(function.effects(), Effects::NONE, "{wrapper}: {dump}");
    }

    let native = emit_and_run_lcir(&artifact, "source-float-conversions");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert!(native.ir.contains("sitofp i64"), "{}", native.ir);
    assert!(native.ir.contains("fptosi double"), "{}", native.ir);
    let conversion = emitted_lcir_function(&native.ir, &artifact, "std.float.to_int");
    let guarded_branch = conversion
        .find("br i1")
        .unwrap_or_else(|| panic!("missing conversion guard:\n{conversion}"));
    let success = conversion
        .find("convert.float_to_int.success:")
        .unwrap_or_else(|| panic!("missing guarded success block:\n{conversion}"));
    let conversion_instruction = conversion
        .find("fptosi double")
        .unwrap_or_else(|| panic!("missing guarded fptosi:\n{conversion}"));
    assert!(
        guarded_branch < success && success < conversion_instruction,
        "fptosi escaped its checked success block:\n{conversion}"
    );
    for forbidden in [
        "%loom.Value",
        "ValueNode",
        "loom_executor_",
        "loom_runtime_int_to_float",
        "loom_runtime_float_to_int",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "conversion IR retained `{forbidden}`:\n{}",
            native.ir
        );
    }

    let checked_mir = prepare_and_run_checked_mir_with_ir(
        &program,
        EmitOptions::run("main"),
        "checked-mir-float-conversions",
    );
    assert!(
        checked_mir.output.status.success(),
        "{:?}",
        checked_mir.output
    );
    assert_eq!(checked_mir.output.stdout, native.output.stdout);
    assert!(
        checked_mir.output.stderr.is_empty(),
        "{:?}",
        checked_mir.output
    );
    assert!(checked_mir.ir.contains("sitofp i64"), "{}", checked_mir.ir);
    let checked_conversion =
        emitted_checked_mir_function(&checked_mir.ir, &program, "std.float.to_int");
    let checked_guard = checked_conversion
        .find("br i1 %convert.float_to_int.valid")
        .unwrap_or_else(|| panic!("missing checked-MIR conversion guard:\n{checked_conversion}"));
    let checked_success = checked_conversion
        .find("\nconvert.float_to_int.success.")
        .unwrap_or_else(|| panic!("missing checked-MIR success block:\n{checked_conversion}"));
    let checked_fptosi = checked_conversion
        .find("fptosi double")
        .unwrap_or_else(|| panic!("missing checked-MIR fptosi:\n{checked_conversion}"));
    assert!(
        checked_guard < checked_success && checked_success < checked_fptosi,
        "checked-MIR fptosi escaped its guarded success block:\n{checked_conversion}"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate covers canonical formatting, moving roots, runtime status integrity, and portable objects"
)]
fn typed_float_formatting_matches_all_backends_and_preserves_moving_text() {
    let pressure = "x".repeat(40 * 1024);
    let source = format!(
        r#"import std.float.format_float

fn join(left Text, right Text) Text {{ left.concat(right) }}

fn render(value Float) Text {{ format_float(value) }}

pub fn main() {{
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-float-format");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
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
fn negative_duration_fault_matches_interpreter_and_checked_mir() {
    let source = r"import std.time.milliseconds

pub fn main() {
    discard milliseconds(-1)
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
    let checked_mir =
        emit_and_run_checked_mir_machine_fault(&program, "main", "checked-mir-negative-duration");
    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    assert!(!checked_mir.status.success(), "{checked_mir:?}");
    let lcir_fault = machine_fault(&lcir.output);
    let checked_mir_fault = machine_fault(&checked_mir);
    assert_eq!(lcir_fault["fault"]["code"], "InvalidDuration");
    assert_eq!(
        lcir_fault["fault"]["message"],
        "Duration milliseconds cannot be negative"
    );
    assert_eq!(
        checked_mir_fault["fault"]["code"],
        interpreted["fault"]["code"]
    );
    assert_eq!(
        checked_mir_fault["fault"]["message"],
        interpreted["fault"]["message"]
    );
    assert_fallible_surface(&lcir.ir);
}

#[test]
fn invalid_duration_during_cleanup_cannot_replace_the_primary_fault() {
    let source = r"import std.time.milliseconds

pub fn main() {
    defer {
        discard milliseconds(-1)
    }
    discard 1 / 0
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
    let checked_mir = emit_and_run_checked_mir_machine_fault(
        &program,
        "main",
        "checked-mir-duration-cleanup-primary",
    );
    let lcir_fault = machine_fault(&lcir.output);
    let checked_mir_fault = machine_fault(&checked_mir);
    assert_eq!(lcir_fault["code"], interpreted["fault"]["code"]);
    assert_eq!(
        checked_mir_fault["fault"]["code"],
        interpreted["fault"]["code"]
    );
}

#[test]
fn pure_immortal_text_operations_need_no_active_runtime_gc_or_executor() {
    let program = compile_source(
        r#"fn inspect(value Text) Bool {
    value.length() == 6 && value.contains("界") && value == "hello界" && value != "other"
}

pub fn main() {
    discard inspect("hello界")
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
    assert_stateless_direct_lcir_surface(&native.ir);
}

#[test]
fn immortal_text_uses_one_pointer_and_allocation_free_runtime_abi_on_all_targets() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-text/main.loom"),
        include_str!("../../../fixtures/lcir-text/main_test.loom"),
    );
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-immortal-text");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
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
    assert_stateless_direct_lcir_surface(&native.ir);

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
        assert_stateless_direct_lcir_surface(&ir);
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate keeps the complete existing Bytes API, managed roots, direct byte operations, and cross-target objects together"
)]
fn managed_bytes_close_the_typed_lcir_route_on_all_supported_targets() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-managed-bytes/main.loom"),
        include_str!("../../../fixtures/lcir-managed-bytes/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    assert_interpreted_tests_pass(&program);

    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(artifact.program());
    let bytes = program.prelude.bytes.expect("canonical Bytes type");
    let bytes_identity = format!("Nominal#{}[] =>", bytes.0);
    assert!(
        dump.contains(&bytes_identity),
        "missing `{bytes_identity}`:\n{dump}"
    );
    for required in [
        "managed_ptr",
        "text.encode_utf8",
        "bytes.length",
        "bytes.get",
        "bytes.append",
        "bytes.decode_utf8",
        "bytes.compare.equal",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    let verifier = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("verifyBytes"))
        .expect("Bytes verifier instance");
    assert!(verifier.effects().contains(Effects::MAY_COLLECT));
    assert!(verifier.effects().contains(Effects::NEEDS_RUNTIME));
    assert!(verifier.effects().contains(Effects::MAY_FAULT));
    assert!(!verifier.effects().contains(Effects::NEEDS_EXECUTOR));
    assert!(!verifier.effects().contains(Effects::MAY_SUSPEND));
    let equality = artifact
        .functions()
        .iter()
        .find(|function| function.name() == "$structuralEquality")
        .expect("Bytes equality helper");
    assert!(equality.effects().is_empty());

    let native = emit_and_run_lcir(&artifact, "source-managed-bytes-tests");
    let checked_mir = emit_and_run_checked_mir_tests(&program, "checked-mir-managed-bytes-tests");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
    for required in [
        "declare i32 @loom_runtime_bytes_append_typed_v1(ptr, ptr, ptr)",
        "declare i32 @loom_runtime_bytes_decode_utf8_typed_v1(ptr, ptr)",
        "declare i32 @memcmp(ptr, ptr, i64)",
        "bytes.get.in_bounds",
        "bytes.get.pointer = getelementptr i8",
        "bytes.decode_utf8.status",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            native.ir.contains(required),
            "missing `{required}`:\n{}",
            native.ir
        );
    }
    assert!(!native.ir.contains("loom_gc_root_push_v1"), "{}", native.ir);
    assert!(!native.ir.contains("loom_executor_"), "{}", native.ir);
    assert!(!native.ir.contains("%loom.Value"), "{}", native.ir);

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create managed Bytes target directory");
        let object = directory.path().join(if target.contains("windows") {
            "managed-bytes.obj"
        } else {
            "managed-bytes.o"
        });
        let ir_path = directory.path().join("managed-bytes.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit managed Bytes object for {target}: {error}"));
        assert!(
            object.is_file(),
            "missing managed Bytes object for {target}"
        );
        let ir = std::fs::read_to_string(ir_path).expect("read managed Bytes target IR");
        for required in [
            "loom_runtime_bytes_append_typed_v1",
            "loom_runtime_bytes_decode_utf8_typed_v1",
            "loom_gc_typed_root_push_v1",
            "@memcmp",
        ] {
            assert!(
                ir.contains(required),
                "{target} omitted `{required}`:\n{ir}"
            );
        }
        assert!(
            ir.contains(&format!("target triple = \"{target}\"")),
            "{ir}"
        );
        assert!(!ir.contains("loom_gc_root_push_v1"), "{ir}");
        assert!(!ir.contains("loom_executor_"), "{ir}");
        assert!(!ir.contains("%loom.Value"), "{ir}");
    }
}

#[test]
fn text_from_utf8_units_is_direct_typed_lcir_on_all_supported_targets() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-text-from-utf8-units/main.loom"),
        include_str!("../../../fixtures/lcir-text-from-utf8-units/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    assert_interpreted_tests_pass(&program);

    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(artifact.program());
    let decode_text_error = artifact
        .program()
        .as_program()
        .canonical_types()
        .decode_text_error
        .expect("canonical DecodeTextError identity");
    for required in ["List[Int] =>", "managed_ptr", "text.from_utf8_units"] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    assert_dump_has_nominal(&dump, decode_text_error.0);
    let verifier = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("verifyMovingSource"))
        .expect("UTF-8-unit relocation verifier");
    assert!(verifier.effects().contains(Effects::MAY_COLLECT));
    assert!(verifier.effects().contains(Effects::NEEDS_RUNTIME));
    assert!(!verifier.effects().contains(Effects::NEEDS_EXECUTOR));

    let native = emit_and_run_lcir(&artifact, "source-text-from-utf8-units-tests");
    assert!(native.output.status.success(), "{:?}", native.output);
    for required in [
        "declare i32 @loom_runtime_text_from_utf8_units_typed_v1(ptr, i64, ptr)",
        "text.from_utf8_units.status",
        "text.from_utf8_units.data.empty",
        "text.from_utf8_units.data.present",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            native.ir.contains(required),
            "missing `{required}`:\n{}",
            native.ir
        );
    }
    assert_typed_lcir_surface(&native.ir);
    assert!(!native.ir.contains("loom_executor_"), "{}", native.ir);

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create UTF-8-unit target directory");
        let object = directory.path().join(if target.contains("windows") {
            "text-from-utf8-units.obj"
        } else {
            "text-from-utf8-units.o"
        });
        let ir_path = directory.path().join("text-from-utf8-units.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit Text.from_utf8_units object for {target}: {error}"));
        assert!(object.is_file(), "missing UTF-8-unit object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read UTF-8-unit target IR");
        for required in [
            "loom_runtime_text_from_utf8_units_typed_v1",
            "loom_gc_typed_root_push_v1",
            "loom_gc_typed_root_pop_v1",
        ] {
            assert!(
                ir.contains(required),
                "{target} omitted `{required}`:\n{ir}"
            );
        }
        assert!(
            ir.contains(&format!("target triple = \"{target}\"")),
            "{ir}"
        );
        assert_typed_lcir_surface(&ir);
        assert!(!ir.contains("loom_executor_"), "{ir}");
    }
}

#[test]
fn lexical_path_is_direct_typed_lcir_on_all_supported_targets() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-path/main.loom"),
        include_str!("../../../fixtures/lcir-typed-path/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    assert_interpreted_tests_pass(&program);

    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(artifact.program());
    let canonical_types = artifact.program().as_program().canonical_types();
    let path = canonical_types.path.expect("canonical Path identity");
    let path_error = canonical_types
        .path_error
        .expect("canonical PathError identity");
    for required in ["path.from_text", "path.as_text", "path.join"] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    for expected in [path, path_error] {
        assert_dump_has_nominal(&dump, expected.0);
    }
    let verifier = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("verifyMovingJoin"))
        .expect("typed Path relocation verifier");
    assert!(verifier.effects().contains(Effects::MAY_COLLECT));
    assert!(verifier.effects().contains(Effects::NEEDS_RUNTIME));
    assert!(!verifier.effects().contains(Effects::NEEDS_EXECUTOR));

    let native = emit_and_run_lcir(&artifact, "source-typed-path-tests");
    assert!(native.output.status.success(), "{:?}", native.output);
    for required in [
        "declare i32 @loom_runtime_path_join_typed_v1(ptr, ptr, ptr)",
        "declare i32 @loom_runtime_text_contains(ptr, i64, ptr, i64)",
        "path.from_text.result",
        "path.join.status",
        "loom_gc_typed_root_push_v1",
        "loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            native.ir.contains(required),
            "missing `{required}`:\n{}",
            native.ir
        );
    }
    for forbidden in [
        "loom_runtime_path_join(",
        "loom_runtime_path_contains_nul",
        "loom_executor_",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "typed Path exposed `{forbidden}`:\n{}",
            native.ir
        );
    }
    assert_typed_lcir_surface(&native.ir);

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create typed Path target directory");
        let object = directory.path().join(if target.contains("windows") {
            "typed-path.obj"
        } else {
            "typed-path.o"
        });
        let ir_path = directory.path().join("typed-path.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit typed Path object for {target}: {error}"));
        assert!(object.is_file(), "missing typed Path object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read typed Path target IR");
        for required in [
            "loom_runtime_path_join_typed_v1",
            "loom_runtime_text_contains",
            "loom_gc_typed_root_push_v1",
            "loom_gc_typed_root_pop_v1",
        ] {
            assert!(
                ir.contains(required),
                "{target} omitted `{required}`:\n{ir}"
            );
        }
        assert!(
            ir.contains(&format!("target triple = \"{target}\"")),
            "{ir}"
        );
        assert_typed_lcir_surface(&ir);
        assert!(!ir.contains("loom_runtime_path_join("), "{ir}");
        assert!(!ir.contains("loom_runtime_path_contains_nul"), "{ir}");
        assert!(!ir.contains("loom_executor_"), "{ir}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one source fixture keeps interpreter/native semantics, forced relocation, IR shape, and cross-target objects in one differential gate"
)]
fn managed_text_concat_runs_tests_reloads_roots_and_emits_on_all_supported_targets() {
    let pressure = "x".repeat(40 * 1024);
    let source = r#"enum Problem { WrongText }

fn join(left Text, right Text) Text { left.concat(right) }

pub fn main() {
    discard join("hello", "界").length()
}
"#;
    let test_source = format!(
        r#"test fn concatMovesAndAliases() Result[Unit, Problem] {{
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
    let program = compile_sources(source, &test_source);
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
    let source = r#"enum Problem { WrongText }

fn join(left Text, right Text) Text { left.concat(right) }

fn select(text Text, index Int) Option[Text] { text.get(index) }

fn equals(input Option[Text], expected Text) Bool {
    match input {
        Some(value) => value == expected
        None => false
    }
}

fn missing(input Option[Text]) Bool {
    match input {
        Some(_) => false
        None => true
    }
}

pub fn main() {
    discard "a界🙂z".get(1)
}
"#;
    let test_source = format!(
        r#"test fn selectsUnicodeScalars() Result[Unit, Problem] {{
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
"#
    );
    let program = compile_sources(source, &test_source);
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
    let checked_mir = emit_and_run_checked_mir_tests(&program, "checked-mir-managed-text-get");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
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
        r#"enum Problem {{ WrongText }}

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
    method refresh(mut self) {{
        let pressure = collectPressure()
        discard pressure.length()
        self.pair.left = self.pair.left.concat("")
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

pub fn main() {{
    discard verify()
}}
"#
    );
    let program = compile_sources(
        &source,
        "test fn managedProducts() Result[Unit, Problem] { verify() }\n",
    );
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
    let program = compile_sources(
        &source,
        include_str!("../../../fixtures/lcir-managed-sums/main_test.loom"),
    );
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
    let program = compile_sources(
        &source,
        include_str!("../../../fixtures/lcir-managed-lists/main_test.loom"),
    );
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
        "pub fn main() {\n    let values = [1, 2]\n    discard values.length()\n}\n",
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
fn typed_logging_interpreter_child() {
    let Some(mode) = std::env::var_os(TYPED_LOGGING_INTERPRETER_CHILD_ENV) else {
        return;
    };
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-logging/main.loom"),
        include_str!("../../../fixtures/lcir-typed-logging/main_test.loom"),
    );
    if mode == "unwritable" {
        let failure =
            interpret_run(&program, "main").expect_err("logging must report stderr failure");
        assert!(
            matches!(failure, ExecutionFailure::Runtime { ref fault } if fault.code == "LogWriteFault" && fault.message == "log write failed"),
            "{failure:#?}"
        );
    } else if mode == "run" {
        assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    } else {
        assert_eq!(mode, "tests");
        let results = Interpreter::new(&program).run_tests();
        assert_eq!(results.len(), 1, "{results:#?}");
        assert_eq!(results[0].status, TestStatus::Passed, "{results:#?}");
    }
}

#[test]
fn typed_stdout_interpreter_child() {
    if std::env::var_os(TYPED_STDOUT_INTERPRETER_CHILD_ENV).is_none() {
        return;
    }
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-stdout/main.loom"),
        include_str!("../../../fixtures/lcir-typed-stdout/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
}

#[test]
fn finite_dynamic_projected_fault_interpreter_child() {
    if std::env::var_os(FINITE_DYN_FAULT_INTERPRETER_CHILD_ENV).is_none() {
        return;
    }
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-dyn-finite/main.loom"),
        include_str!("../../../fixtures/lcir-dyn-finite/main_test.loom"),
    );
    let failure = interpret_run(&program, "projectedFaultMain")
        .expect_err("projected mutable method must retain its primary fault");
    assert!(
        matches!(failure, ExecutionFailure::Contract { ref fault } if fault.code == "AssertionFault"),
        "{failure:#?}"
    );
}

#[test]
fn source_std_io_writes_exact_text_through_typed_lcir() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-stdout/main.loom"),
        include_str!("../../../fixtures/lcir-typed-stdout/main_test.loom"),
    );

    let interpreter = Command::new(std::env::current_exe().expect("current LCIR test executable"))
        .args(["--exact", "typed_stdout_interpreter_child", "--nocapture"])
        .env(TYPED_STDOUT_INTERPRETER_CHILD_ENV, "1")
        .output()
        .expect("run typed stdout interpreter child");
    assert!(interpreter.status.success(), "{interpreter:?}");
    assert!(
        String::from_utf8_lossy(&interpreter.stdout).contains("loom stdout 界\n"),
        "{interpreter:?}"
    );

    for policy in [NativeRoutePolicy::Automatic, NativeRoutePolicy::LcirOnly] {
        let prepared = prepare_native_object(&program, EmitOptions::run("main"), policy)
            .unwrap_or_else(|error| panic!("prepare source std.io with {policy:?}: {error}"));
        assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    }

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert_eq!(dump.matches("stdout.write ").count(), 2, "{dump}");
    for function in ["std.io.write", "std.io.write_line"] {
        assert!(dump.contains(function), "{dump}");
    }
    let entry = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("typed stdout entry");
    assert!(entry.effects().contains(Effects::MAY_FAULT));
    assert!(entry.effects().contains(Effects::MAY_COLLECT));
    assert!(entry.effects().contains(Effects::NEEDS_RUNTIME));
    assert!(!entry.effects().contains(Effects::NEEDS_EXECUTOR));

    let native = emit_and_run_lcir_with_options(
        &artifact,
        "source-typed-stdout",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, "loom stdout 界\nUnit\n".as_bytes());
    assert!(native.output.stderr.is_empty(), "{:?}", native.output);
    for required in [
        "@loom_runtime_stdout_write_v1",
        "StdoutWriteFault",
        "stdout.write.failed",
        "llvm.trap",
    ] {
        assert!(
            native.ir.contains(required),
            "missing `{required}`:\n{}",
            native.ir
        );
    }
    for forbidden in ["%loom.Value", "ValueNode", "loom_executor_"] {
        assert!(
            !native.ir.contains(forbidden),
            "retained `{forbidden}`:\n{}",
            native.ir
        );
    }

    let directory = tempfile::tempdir().expect("create unwritable stdout output");
    let object = directory.path().join("typed-stdout-unwritable.o");
    let executable = directory.path().join("typed-stdout-unwritable");
    emit_lcir_native_object(
        &artifact,
        &object,
        &NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    )
    .expect("emit typed stdout failure fixture");
    link_native_object(&object, &executable).expect("link typed stdout failure fixture");
    let read_only_path = directory.path().join("read-only-stdout");
    std::fs::write(&read_only_path, b"stdout sentinel\n").expect("create stdout sentinel");
    let read_only = std::fs::File::open(&read_only_path).expect("open read-only stdout");
    let failed = Command::new(&executable)
        .env(FAULT_FORMAT_ENV, FAULT_FORMAT_JSON)
        .stdout(Stdio::from(read_only))
        .stderr(Stdio::piped())
        .output()
        .expect("run typed stdout with read-only stdout");
    assert!(!failed.status.success(), "{failed:?}");
    let fault = machine_fault(&failed);
    assert_eq!(fault["channel"], "runtime");
    assert_eq!(fault["fault"]["code"], "StdoutWriteFault");
    assert_eq!(fault["fault"]["message"], "standard output write failed");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one gate keeps five public logging calls over ordinary source wrappers, exact interpreter/typed stderr, release IR purity, and Linux/MSVC object ABIs together"
)]
fn typed_logging_uses_one_direct_fallible_runtime_boundary() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-logging/main.loom"),
        include_str!("../../../fixtures/lcir-typed-logging/main_test.loom"),
    );

    for mode in ["run", "tests"] {
        let output = Command::new(std::env::current_exe().expect("current LCIR test executable"))
            .args(["--exact", "typed_logging_interpreter_child", "--nocapture"])
            .env(TYPED_LOGGING_INTERPRETER_CHILD_ENV, mode)
            .output()
            .unwrap_or_else(|error| panic!("run typed logging interpreter {mode}: {error}"));
        assert!(output.status.success(), "interpreter {mode}: {output:?}");
        assert_eq!(output.stderr, TYPED_LOGGING_STDERR, "interpreter {mode}");
    }

    for (options, request) in [
        (
            EmitOptions::run("main"),
            SourceArtifactRequest::Run {
                entry: "main".into(),
            },
        ),
        (EmitOptions::tests(), SourceArtifactRequest::Tests),
    ] {
        for policy in [NativeRoutePolicy::Automatic, NativeRoutePolicy::LcirOnly] {
            let prepared = prepare_native_object(&program, options.clone(), policy)
                .unwrap_or_else(|error| panic!("prepare typed logging with {policy:?}: {error}"));
            assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
        }

        let artifact = lower_source_artifact(&program, &request);
        let dump = dump_program(artifact.program());
        assert_eq!(dump.matches("log.write ").count(), 1, "{dump}");
        for function in [
            "debug",
            "info",
            "warn",
            "error",
            "write",
            "write_without_fields",
        ] {
            assert!(dump.contains(&format!("std.log.{function}")), "{dump}");
        }
        let logging = artifact
            .functions()
            .iter()
            .find(|function| function.name().ends_with("emitCanonicalLogs"))
            .expect("typed logging source function");
        assert!(logging.effects().contains(Effects::MAY_FAULT));
        assert!(logging.effects().contains(Effects::MAY_COLLECT));
        assert!(logging.effects().contains(Effects::NEEDS_RUNTIME));
        assert!(!logging.effects().contains(Effects::NEEDS_EXECUTOR));

        let stem = match &request {
            SourceArtifactRequest::Run { .. } => "typed-logging-run",
            SourceArtifactRequest::Tests => "typed-logging-tests",
        };
        let native = emit_and_run_lcir_with_options(
            &artifact,
            stem,
            NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
        );
        assert!(native.output.status.success(), "{:#?}", native.output);
        assert_eq!(native.output.stderr, TYPED_LOGGING_STDERR);
        for required in [
            "@loom_runtime_log_typed_v1",
            "LogWriteFault",
            "log.write.failed",
            "llvm.trap",
        ] {
            assert!(
                native.ir.contains(required),
                "typed logging omitted `{required}`:\n{}",
                native.ir
            );
        }
        for forbidden in [
            "@loom_runtime_log(",
            "%loom.Value",
            "ValueNode",
            "loom_runtime_text_map_",
            "@loom_gc_root_push_v1",
            "loom_executor_",
        ] {
            assert!(
                !native.ir.contains(forbidden),
                "typed logging retained `{forbidden}`:\n{}",
                native.ir
            );
        }

        for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
            let directory = tempfile::tempdir().expect("create typed logging target directory");
            let object = directory.path().join(if target.contains("windows") {
                "typed-logging.obj"
            } else {
                "typed-logging.o"
            });
            let ir_path = directory.path().join("typed-logging.ll");
            emit_lcir_native_object(
                &artifact,
                &object,
                &NativeObjectOptions {
                    target_triple: Some(target.to_owned()),
                    emit_ir: Some(ir_path.clone()),
                    optimization: OptimizationProfile::Release,
                    ..NativeObjectOptions::default()
                },
            )
            .unwrap_or_else(|error| panic!("emit typed logging object for {target}: {error}"));
            let bytes = std::fs::read(&object).expect("read typed logging target object");
            if target.contains("windows") {
                assert_eq!(bytes.get(..2), Some([0x64, 0x86].as_slice()));
            } else {
                assert_eq!(bytes.get(..4), Some(b"\x7fELF".as_slice()));
            }
            let ir = std::fs::read_to_string(ir_path).expect("read typed logging target IR");
            assert!(ir.contains("@loom_runtime_log_typed_v1"), "{target}: {ir}");
            assert!(!ir.contains("@loom_runtime_log("), "{target}: {ir}");
            assert!(!ir.contains("%loom.Value"), "{target}: {ir}");
            assert!(!ir.contains("loom_executor_"), "{target}: {ir}");
        }
    }

    let directory = tempfile::tempdir().expect("create unwritable-stderr logging outputs");
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let typed_object = directory.path().join("typed-logging-unwritable.o");
    let typed_executable = directory.path().join("typed-logging-unwritable");
    emit_lcir_native_object(
        &artifact,
        &typed_object,
        &NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    )
    .expect("emit typed logging unwritable-stderr object");
    link_native_object(&typed_object, &typed_executable)
        .expect("link typed logging unwritable-stderr executable");

    let mut interpreter = Command::new(std::env::current_exe().expect("current test executable"));
    interpreter
        .args(["--exact", "typed_logging_interpreter_child", "--nocapture"])
        .env(TYPED_LOGGING_INTERPRETER_CHILD_ENV, "unwritable");
    let interpreter = run_with_read_only_stderr(&mut interpreter, directory.path());
    assert!(
        interpreter.status.success(),
        "interpreter did not observe read-only stderr: {interpreter:?}"
    );
    let mut command = Command::new(&typed_executable);
    let output = run_with_read_only_stderr(&mut command, directory.path());
    assert!(
        !output.status.success(),
        "{} silently accepted read-only stderr: {output:?}",
        typed_executable.display()
    );

    #[cfg(unix)]
    {
        let mut interpreter =
            Command::new(std::env::current_exe().expect("current test executable"));
        interpreter
            .args(["--exact", "typed_logging_interpreter_child", "--nocapture"])
            .env(TYPED_LOGGING_INTERPRETER_CHILD_ENV, "unwritable");
        let interpreter = run_with_closed_stderr(&mut interpreter);
        assert!(
            interpreter.status.success(),
            "interpreter did not observe closed stderr: {interpreter:?}"
        );
        let mut command = Command::new(&typed_executable);
        let output = run_with_closed_stderr(&mut command);
        assert!(
            !output.status.success(),
            "{} silently accepted closed stderr: {output:?}",
            typed_executable.display()
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one native differential gate keeps typed TextMap value shapes, immutable aliasing, exact lookup, moving-GC roots, and cross-target descriptors together"
)]
fn typed_text_maps_are_direct_exact_and_survive_forced_relocation() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-textmap/main.loom"),
        include_str!("../../../fixtures/lcir-typed-textmap/main_test.loom"),
    );

    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:?}");
    assert_eq!(interpreted[0].status, TestStatus::Passed, "{interpreted:?}");

    let tests_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(tests_artifact.program());
    for required in [
        "text_map.construct",
        "text_map.insert",
        "text_map.length",
        "text_map.contains",
        "text_map.get",
        "text_map.remove",
        "text_map.entry_get",
        "list.construct",
        "list.get",
        "sum.switch",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    assert!(!dump.contains("unsupported"), "{dump}");

    let native_tests = emit_and_run_lcir(&tests_artifact, "source-typed-text-maps-tests");
    assert!(
        native_tests.output.status.success(),
        "{:?}",
        native_tests.output
    );
    assert!(
        String::from_utf8_lossy(&native_tests.output.stdout).contains("typedTextMap"),
        "{:?}",
        native_tests.output
    );
    for required in [
        "loom_gc_typed_repeated_alloc_v1",
        "loom.lcir.text_map.descriptor",
        "loom.lcir.text_map.pointer_offsets",
        "managed.root.reload",
        "llvm.memcpy",
        "text_map.lookup",
        "text_map.remove.source",
        "text_map.entry_get",
        "memcmp",
    ] {
        assert!(
            native_tests.ir.contains(required),
            "typed TextMap IR omitted `{required}`:\n{}",
            native_tests.ir
        );
    }
    for forbidden in [
        "%loom.Value",
        "loom_runtime_text_map_get",
        "loom_runtime_text_map_insert",
        "loom_runtime_text_map_remove",
        "ValueNode",
        "loom_executor_",
        "loom_gc_root_push_v1",
    ] {
        assert!(
            !native_tests.ir.contains(forbidden),
            "typed TextMap IR exposed `{forbidden}`:\n{}",
            native_tests.ir
        );
    }

    let verify = emitted_lcir_function(&native_tests.ir, &tests_artifact, "verify");
    assert!(verify.contains("text_map.insert.copy_source"), "{verify}");
    assert!(verify.contains("text_map.remove.source"), "{verify}");
    assert!(verify.contains("text_map.remove.prefix_bytes"), "{verify}");
    assert!(verify.contains("text_map.remove.suffix_count"), "{verify}");
    assert!(verify.contains("text_map.entry_get"), "{verify}");
    assert!(verify.contains("managed.root.reload"), "{verify}");
    assert!(
        verify.matches("@loom_gc_typed_repeated_alloc_v1").count() >= 15,
        "every insert/remove source site must retain an exact typed allocation:\n{verify}"
    );

    let checked_mir_tests =
        emit_and_run_checked_mir_tests(&program, "checked-mir-typed-text-maps-tests");
    assert_eq!(
        checked_mir_tests.status.success(),
        native_tests.output.status.success()
    );
    assert_eq!(checked_mir_tests.stdout, native_tests.output.stdout);
    assert_eq!(checked_mir_tests.stderr, native_tests.output.stderr);

    let run_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let native_run = emit_and_run_lcir(&run_artifact, "source-typed-text-maps-run");
    assert!(
        native_run.output.status.success(),
        "{:?}",
        native_run.output
    );
    assert_eq!(native_run.output.stdout, b"Unit\n");
    let checked_mir_run =
        emit_and_run_checked_mir(&program, "main", "checked-mir-typed-text-maps-run");
    assert_eq!(
        checked_mir_run.status.success(),
        native_run.output.status.success()
    );
    assert_eq!(checked_mir_run.stdout, native_run.output.stdout);
    assert_eq!(checked_mir_run.stderr, native_run.output.stderr);

    let release_directory = tempfile::tempdir().expect("create release TextMap directory");
    let release_object = release_directory.path().join("typed-text-map-release.o");
    let release_ir_path = release_directory.path().join("typed-text-map-release.ll");
    emit_lcir_native_object(
        &tests_artifact,
        &release_object,
        &NativeObjectOptions {
            emit_ir: Some(release_ir_path.clone()),
            optimization: OptimizationProfile::Release,
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit release typed-TextMap object");
    let release_ir =
        std::fs::read_to_string(release_ir_path).expect("read release typed-TextMap IR");
    assert!(
        release_ir.contains("@loom_gc_typed_repeated_alloc_v1"),
        "release removal lost its conditional exact allocator:\n{release_ir}"
    );
    assert!(
        release_ir.contains("@llvm.memcpy"),
        "release removal lost its exact prefix/suffix copies:\n{release_ir}"
    );
    assert!(
        release_ir.contains("text_map.remove."),
        "release IR lost every remove-specific control/data-flow marker:\n{release_ir}"
    );
    for forbidden in [
        "%loom.Value",
        "loom_runtime_text_map_",
        "ValueNode",
        "loom_executor_",
    ] {
        assert!(
            !release_ir.contains(forbidden),
            "release TextMap IR exposed `{forbidden}`:\n{release_ir}"
        );
    }

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create typed TextMap target directory");
        let object = directory.path().join("typed-text-map.o");
        let ir_path = directory.path().join("typed-text-map.ll");
        emit_lcir_native_object(
            &tests_artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit typed TextMap object for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read typed TextMap target IR");
        for required in [
            "loom_gc_typed_repeated_alloc_v1",
            "loom.lcir.text_map.descriptor",
            "managed.root.reload",
            "llvm.memcpy",
            "text_map.remove.source",
            "text_map.entry_get",
            "memcmp",
        ] {
            assert!(
                ir.contains(required),
                "{target} omitted `{required}`:\n{ir}"
            );
        }
        assert!(!ir.contains("%loom.Value"), "{ir}");
        assert!(!ir.contains("loom_runtime_text_map_"), "{ir}");
        assert!(!ir.contains("loom_executor_"), "{ir}");
    }
}

#[test]
fn std_text_map_segment_classifies_through_direct_lcir() {
    let test_source = include_str!("../../../fixtures/std/main_test.loom")
        .replace("__LOOPBACK_PORT__", "1")
        .replace("__READ_LOOPBACK_PORT__", "1");
    let program = compile_sources(
        include_str!("../../../fixtures/std/main.loom"),
        &test_source,
    );
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "typed_text_map_segment".into(),
        },
    );
    let dump = dump_program(artifact.program());
    for required in [
        "text_map.contains",
        "text_map.remove",
        "text_map.entry_get",
        "text.compare.equal",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    assert!(!dump.contains("unsupported"), "{dump}");
}

#[test]
fn complete_std_tests_route_through_lcir() {
    let test_source = include_str!("../../../fixtures/std/main_test.loom")
        .replace("__ROUND_TRIP_PATH__", "round-trip.txt")
        .replace("__MISSING_PATH__", "missing.txt")
        .replace("__REUSE_PATH__", "reuse.txt")
        .replace("__LOOPBACK_PORT__", "1")
        .replace("__READ_LOOPBACK_PORT__", "1");
    let program = compile_sources(
        include_str!("../../../fixtures/std/main.loom"),
        &test_source,
    );
    for policy in [NativeRoutePolicy::Automatic, NativeRoutePolicy::LcirOnly] {
        let prepared = prepare_native_object(&program, EmitOptions::tests(), policy)
            .unwrap_or_else(|error| {
                panic!("prepare standard-library tests with {policy:?}: {error}")
            });
        assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    }
}

#[test]
fn async_scoped_resource_close_uses_the_checked_executor() {
    let source = r#"import std.file.try_create

pub async fn main() {
    match try_create("scoped-close.txt").await {
        Ok(file) => {
            scoped output = file
        }
        Err(_) => {
            assert false
        }
    }
}
"#;
    let program = compile_source(source);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let directory = tempfile::tempdir().expect("create async scoped-close directory");
    let object = directory.path().join("async-scoped-close.o");
    let ir_path = directory.path().join("async-scoped-close.ll");
    let executable = directory.path().join("async-scoped-close");
    emit_lcir_native_object(
        &artifact,
        &object,
        &NativeObjectOptions {
            emit_ir: Some(ir_path.clone()),
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit async scoped-close object");
    link_native_object(&object, &executable).expect("link async scoped-close executable");
    let output = Command::new(&executable)
        .current_dir(directory.path())
        .output()
        .expect("run async scoped-close executable");
    assert!(output.status.success(), "{output:?}");
    assert!(directory.path().join("scoped-close.txt").is_file());

    let ir = std::fs::read_to_string(ir_path).expect("read async scoped-close LLVM IR");
    assert_typed_resource_close_guard(&ir, &artifact, ResourceKind::File, 1);
    assert_typed_lcir_surface(&ir);
}

#[test]
fn typed_io_tasks_use_direct_result_frames_and_real_scheduler_io() {
    let source = include_str!("../../../fixtures/lcir-typed-io/main.loom");
    let program = compile_source(source);
    for policy in [NativeRoutePolicy::Automatic, NativeRoutePolicy::LcirOnly] {
        let prepared = prepare_native_object(&program, EmitOptions::run("main"), policy)
            .unwrap_or_else(|error| panic!("prepare typed I/O with {policy:?}: {error}"));
        assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    }
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    for operation in [
        "io.task_create.file_open_read.result",
        "io.task_create.file_create.result",
        "io.task_create.file_read_text.result",
        "io.task_create.file_write_text.result",
        "io.task_create.socket_connect.result",
        "io.task_create.socket_read_text.result",
        "io.task_create.socket_write_text.result",
    ] {
        assert!(dump.contains(operation), "missing `{operation}`:\n{dump}");
    }

    let directory = tempfile::tempdir().expect("create typed I/O run directory");
    let object = directory.path().join("typed-io.o");
    let ir_path = directory.path().join("typed-io.ll");
    let executable = directory.path().join("typed-io");
    emit_lcir_native_object(
        &artifact,
        &object,
        &NativeObjectOptions {
            emit_ir: Some(ir_path.clone()),
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit direct typed I/O object");
    link_native_object(&object, &executable).expect("link direct typed I/O executable");
    let output = Command::new(&executable)
        .current_dir(directory.path())
        .output()
        .expect("run direct typed I/O executable");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read(directory.path().join("round-trip.txt")).expect("read I/O result"),
        b"direct typed I/O"
    );
    let ir = std::fs::read_to_string(ir_path).expect("read direct typed I/O LLVM IR");
    for symbol in [
        "loom_typed_io_task_create_v1",
        "loom_typed_io_poll_v1",
        "loom_typed_io_cancel_v1",
        "loom_typed_resource_close_v1",
        "loom_typed_task_publish_result_v1",
    ] {
        assert!(ir.contains(symbol), "missing `{symbol}`:\n{ir}");
    }
    for forbidden in ["loom.Value", "loom_file_try_", "loom_socket_try_"] {
        assert!(
            !ir.contains(forbidden),
            "checked-MIR backend token `{forbidden}` remained:\n{ir}"
        );
    }
    assert_typed_resource_close_guard(&ir, &artifact, ResourceKind::File, 1);
    assert_typed_resource_close_guard(&ir, &artifact, ResourceKind::Socket, 2);
    assert!(
        ir.lines()
            .filter(|line| line.contains("call i32 @loom_typed_resource_close_v1"))
            .all(|line| line.contains("ptr %__loom_executor")),
        "typed scoped cleanup used a non-executor context:\n{ir}"
    );

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let object = directory.path().join(if target.contains("windows") {
            "typed-io.obj"
        } else {
            "typed-io-linux.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit typed I/O object for {target}: {error}"));
        assert!(object.is_file(), "missing typed I/O object for {target}");
    }
}

#[test]
fn fault_mode_typed_io_callbacks_record_operation_specific_faults() {
    let source = r#"import std.file.open_read
import std.file.create
import std.net.connect

pub async fn main() {
    {
        scoped output = create("typed-fault-mode.txt").await
        output.write_text("loom").await
    }
    {
        scoped input = open_read("typed-fault-mode.txt").await
        discard input.read_text().await
    }
    {
        scoped socket = connect("localhost", 1).await
        socket.write_text("loom").await
        discard socket.read_text().await
    }
}
"#;
    let program = compile_source(source);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    for operation in [
        "file_open_read",
        "file_create",
        "file_read_text",
        "file_write_text",
        "socket_connect",
        "socket_read_text",
        "socket_write_text",
    ] {
        let expected = format!("io.task_create.{operation}.fault");
        assert!(dump.contains(&expected), "missing `{expected}`:\n{dump}");
    }

    let directory = tempfile::tempdir().expect("create fault-mode I/O directory");
    let object = directory.path().join("fault-mode-io.o");
    let ir_path = directory.path().join("fault-mode-io.ll");
    emit_lcir_native_object(
        &artifact,
        &object,
        &NativeObjectOptions {
            emit_ir: Some(ir_path.clone()),
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit fault-mode typed I/O object");
    let ir = std::fs::read_to_string(ir_path).expect("read fault-mode typed I/O LLVM IR");
    for code in [
        "FileOpenFault",
        "FileCreateFault",
        "FileReadFault",
        "FileWriteFault",
        "InvalidPort",
        "SocketConnectFault",
        "SocketResolveFault",
        "SocketReadFault",
        "SocketWriteFault",
    ] {
        assert!(ir.contains(code), "missing `{code}`:\n{ir}");
    }
    assert!(ir.contains("@loom_context_raise_fault_v1"), "{ir}");
    assert!(ir.contains("io.fault_mode.resource"), "{ir}");
    assert!(!ir.contains("%loom.Value"), "{ir}");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one gate keeps source JSON call-graph reachability, bulk construction, both harnesses, and runtime purity together"
)]
fn source_json_parser_is_direct_bulk_lcir_in_run_and_test_harnesses() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-json-parse/main.loom"),
        include_str!("../../../fixtures/lcir-json-parse/main_test.loom"),
    );

    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );

    for (options, scenario) in [
        (EmitOptions::run("main"), "run"),
        (EmitOptions::tests(), "tests"),
    ] {
        for policy in [NativeRoutePolicy::Automatic, NativeRoutePolicy::LcirOnly] {
            let prepared =
                prepare_native_object(&program, options.clone(), policy).unwrap_or_else(|error| {
                    panic!("prepare source JSON parser {scenario} with {policy:?}: {error}")
                });
            assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
        }
    }

    let assert_direct_parser = |artifact: &CheckedArtifact, ir: &str, scenario: &str| {
        let dump = dump_program(artifact.program());
        for required in [
            "std.json.parse_json",
            "std.json.parse_value",
            "std.json.finish_object",
            "text_map.construct_entries",
        ] {
            assert!(
                dump.contains(required),
                "source JSON {scenario} LCIR omitted `{required}`:\n{dump}"
            );
        }
        assert_eq!(
            dump.matches("list.append.unique").count(),
            6,
            "every parser-local construction append must stay unique:\n{dump}"
        );
        assert!(
            !dump.contains("list.append %"),
            "source JSON {scenario} regressed to functional loop append:\n{dump}"
        );
        let finish_object = artifact
            .functions()
            .iter()
            .find(|function| function.name().ends_with("finish_object"))
            .expect("source finish_object helper");
        assert!(finish_object.instructions().iter().any(|instruction| {
            matches!(
                instruction.kind(),
                InstructionKind::TextMapConstructEntries { .. }
            )
        }));
        assert!(finish_object.effects().contains(Effects::MAY_COLLECT));
        assert!(!finish_object.effects().contains(Effects::NEEDS_EXECUTOR));
        for required in [
            "@loom_gc_typed_repeated_alloc_v1",
            "text_map.bulk.heap.header",
            "text_map.bulk.sort.header",
            "text_map.bulk.scan.header",
            "text_map.bulk.duplicate",
            "llvm.memcpy",
            "managed.root.reload",
        ] {
            assert!(
                ir.contains(required),
                "source JSON {scenario} IR omitted `{required}`:\n{ir}"
            );
        }
        for forbidden in [
            "%loom.Value",
            "loom_runtime_text_map_insert",
            "ValueNode",
            "loom.fn",
            "loom_executor_",
            "@qsort",
        ] {
            assert!(
                !ir.contains(forbidden),
                "source JSON {scenario} retained `{forbidden}`:\n{ir}"
            );
        }
    };

    let run_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let native_run = emit_and_run_lcir(&run_artifact, "source-json-parser-run");
    assert!(
        native_run.output.status.success(),
        "{:#?}",
        native_run.output
    );
    assert_eq!(native_run.output.stdout, b"Unit\n");
    assert!(
        native_run.output.stderr.is_empty(),
        "{:#?}",
        native_run.output
    );
    assert_direct_parser(&run_artifact, &native_run.ir, "run");
    let checked_mir_run = prepare_and_run_checked_mir(
        &program,
        EmitOptions::run("main"),
        "checked-mir-source-json-parser-run",
    );
    assert!(checked_mir_run.status.success(), "{checked_mir_run:#?}");
    assert_eq!(checked_mir_run.stdout, native_run.output.stdout);
    assert_eq!(checked_mir_run.stderr, native_run.output.stderr);

    let tests_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native_tests = emit_and_run_lcir(&tests_artifact, "source-json-parser-tests");
    assert!(
        native_tests.output.status.success(),
        "{:#?}",
        native_tests.output
    );
    assert!(
        String::from_utf8_lossy(&native_tests.output.stdout)
            .contains("passed standalone.source_json_parse"),
        "{:#?}",
        native_tests.output
    );
    assert!(
        native_tests.output.stderr.is_empty(),
        "{:#?}",
        native_tests.output
    );
    assert_direct_parser(&tests_artifact, &native_tests.ir, "tests");
    let checked_mir_tests = prepare_and_run_checked_mir(
        &program,
        EmitOptions::tests(),
        "checked-mir-source-json-parser-tests",
    );
    assert!(checked_mir_tests.status.success(), "{checked_mir_tests:#?}");
    assert_eq!(checked_mir_tests.stdout, native_tests.output.stdout);
    assert_eq!(checked_mir_tests.stderr, native_tests.output.stderr);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one gate keeps multi-leaf moving-GC behavior and both cross-target object surfaces together"
)]
fn text_map_bulk_copy_sort_preserves_source_alias_across_moving_collection() {
    let half = "x".repeat(20 * 1024);
    let source = format!(
        r#"record Pair {{
    left Text
    right Text
}}

fn first_entry_is_valid(entries List[(Text, Pair)], left Text, right Text) Bool {{
    match entries.get(0) {{
        Some(entry) => {{
            let key, value = entry
            key == "z" && value.left == left && value.right == right
        }}
        None => false
    }}
}}

fn verify(left Text, right Text) Bool {{
    let value = Pair {{ left = left, right = right }}
    var entries = List[(Text, Pair)]()
    entries.add(("z", value))
    entries.add(("a", value))
    let alias = entries
    let pressure = left.concat(right)
    discard pressure
    match entries.to_text_map() {{
        Ok(map) => {{
            let first_valid = match map.entry_at(0) {{
                Some(entry) => {{
                    let key, value = entry
                    key == "a" && value.left == left && value.right == right
                }}
                None => false
            }}
            first_entry_is_valid(alias, left, right) && map.length() == 2 && first_valid
        }}
        Err(_) => false
    }}
}}

pub fn main() {{
    let left = "{half}".concat("{half}")
    let right = left.concat("!")
    let valid = verify(left, right)
    assert valid
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
    let dump = dump_program(artifact.program());
    assert_eq!(
        dump.matches("text_map.construct_entries").count(),
        1,
        "{dump}"
    );
    assert_eq!(dump.matches("list.append.unique").count(), 2, "{dump}");
    assert!(!dump.contains("list.append %"), "{dump}");

    let native = emit_and_run_lcir(&artifact, "source-text-map-bulk-relocation");
    assert!(native.output.status.success(), "{:#?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert!(native.output.stderr.is_empty(), "{:#?}", native.output);
    let verify = emitted_lcir_function(&native.ir, &artifact, "verify");
    for required in [
        "text_map.bulk.allocate",
        "text_map.bulk.source",
        "text_map.bulk.destination",
        "text_map.bulk.sort.header",
        "managed.root.reload",
        "loom_gc_typed_root_push_v1",
        "@loom_gc_typed_repeated_alloc_v1",
        "@llvm.memcpy",
    ] {
        assert!(verify.contains(required), "missing `{required}`:\n{verify}");
    }

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create TextMap bulk target directory");
        let object = directory.path().join(if target.contains("windows") {
            "text-map-bulk.obj"
        } else {
            "text-map-bulk.o"
        });
        let ir_path = directory.path().join("text-map-bulk.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit TextMap bulk object for {target}: {error}"));
        let bytes = std::fs::read(&object).expect("read TextMap bulk target object");
        if target.contains("windows") {
            assert_eq!(bytes.get(..2), Some([0x64, 0x86].as_slice()));
        } else {
            assert_eq!(bytes.get(..4), Some(b"\x7fELF".as_slice()));
        }
        let ir = std::fs::read_to_string(ir_path).expect("read TextMap bulk target IR");
        assert!(
            ir.contains(&format!("target triple = \"{target}\"")),
            "TextMap bulk object used the wrong target:\n{ir}"
        );
        let verify = emitted_lcir_function(&ir, &artifact, "verify");
        for required in [
            "text_map.bulk.source",
            "text_map.bulk.sort.header",
            "managed.root.reload",
        ] {
            assert!(
                verify.contains(required),
                "{target} omitted `{required}`:\n{verify}"
            );
        }
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one source gate keeps the direct typed JSON boundary, run/test harnesses, runtime purity, and Linux/MSVC object ABIs together"
)]
fn typed_json_format_uses_one_direct_collecting_runtime_boundary() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-json-format/main.loom"),
        include_str!("../../../fixtures/lcir-json-format/main_test.loom"),
    );

    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );

    for (options, scenario) in [
        (EmitOptions::run("main"), "run"),
        (EmitOptions::tests(), "tests"),
    ] {
        for policy in [NativeRoutePolicy::Automatic, NativeRoutePolicy::LcirOnly] {
            let prepared =
                prepare_native_object(&program, options.clone(), policy).unwrap_or_else(|error| {
                    panic!("prepare typed JSON format {scenario} with {policy:?}: {error}")
                });
            assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
        }
    }

    let run_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(run_artifact.program());
    assert_eq!(dump.matches("json.format").count(), 5, "{dump}");
    let formatter = run_artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("formats"))
        .expect("typed JSON formatter helper");
    assert!(
        formatter
            .instructions()
            .iter()
            .any(|instruction| matches!(instruction.kind(), InstructionKind::JsonFormat { .. }))
    );
    assert!(formatter.effects().contains(Effects::MAY_COLLECT));
    assert!(formatter.effects().contains(Effects::NEEDS_RUNTIME));
    assert!(!formatter.effects().contains(Effects::MAY_FAULT));
    assert!(!formatter.effects().contains(Effects::NEEDS_EXECUTOR));
    assert!(!formatter.effects().contains(Effects::MAY_SUSPEND));

    let assert_direct_json_ir = |ir: &str, scenario: &str| {
        for required in [
            TYPED_JSON_FORMAT_SYMBOL,
            "@loom.lcir.typed_json.layout.",
            "json.format.status",
            "managed.root.reload",
            "loom_gc_typed_root_push_v1",
            "loom_gc_typed_root_pop_v1",
        ] {
            assert!(
                ir.contains(required),
                "typed JSON {scenario} IR omitted `{required}`:\n{ir}"
            );
        }
        for forbidden in [
            "@loom_runtime_json_format(",
            "%loom.Value",
            "ArgNode",
            "ValueNode",
            "@loom_gc_root_push_v1",
            "loom_executor_",
        ] {
            assert!(
                !ir.contains(forbidden),
                "typed JSON {scenario} IR retained `{forbidden}`:\n{ir}"
            );
        }
    };

    let native_run = emit_and_run_lcir(&run_artifact, "source-typed-json-format-run");
    assert!(
        native_run.output.status.success(),
        "{:#?}",
        native_run.output
    );
    assert_eq!(native_run.output.stdout, b"Unit\n");
    assert!(
        native_run.output.stderr.is_empty(),
        "{:#?}",
        native_run.output
    );
    assert_direct_json_ir(&native_run.ir, "run");

    let tests_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native_tests = emit_and_run_lcir(&tests_artifact, "source-typed-json-format-tests");
    assert!(
        native_tests.output.status.success(),
        "{:#?}",
        native_tests.output
    );
    assert!(
        String::from_utf8_lossy(&native_tests.output.stdout)
            .contains("passed standalone.typedJsonFormat"),
        "{:#?}",
        native_tests.output
    );
    assert!(
        native_tests.output.stderr.is_empty(),
        "{:#?}",
        native_tests.output
    );
    assert_direct_json_ir(&native_tests.ir, "tests");

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create typed JSON format target directory");
        let object = directory.path().join(if target.contains("windows") {
            "typed-json-format.obj"
        } else {
            "typed-json-format.o"
        });
        let ir_path = directory.path().join("typed-json-format.ll");
        emit_lcir_native_object(
            &tests_artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit typed JSON format object for {target}: {error}"));
        let bytes = std::fs::read(&object).expect("read typed JSON format target object");
        if target.contains("windows") {
            assert_eq!(bytes.get(..2), Some([0x64, 0x86].as_slice()));
        } else {
            assert_eq!(bytes.get(..4), Some(b"\x7fELF".as_slice()));
        }
        let ir = std::fs::read_to_string(ir_path).expect("read typed JSON format target IR");
        assert_direct_json_ir(&ir, target);
    }
}

#[test]
fn typed_json_format_hoists_loop_storage_and_reloads_after_moving_collection() {
    let half = "x".repeat(20 * 1024);
    let source = format!(
        r#"import std.json.format_json

fn verify(value Json, expectedLength Int) Bool {{
    var valid = true
    for index in 0..3 {{
        let current = match format_json(value) {{
            Ok(text) => text.length() == expectedLength
            Err(_) => false
        }}
        valid = valid && current
    }}
    valid && match value {{
        Json.Object(fields) => match fields.get("value") {{
            Some(Json.Text(text)) => text.length() == 40960
            _ => false
        }}
        _ => false
    }}
}}

pub fn main() {{
    let payload = "{half}".concat("{half}")
    let value = Json.Object(TextMap[Json]().insert("value", Json.Text(payload)))
    let valid = verify(value, 40972)
    assert valid
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
    let native = emit_and_run_lcir(&artifact, "source-typed-json-format-relocation");
    assert!(native.output.status.success(), "{:#?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");
    assert!(native.output.stderr.is_empty(), "{:#?}", native.output);

    let verify = emitted_lcir_function(&native.ir, &artifact, "verify");
    let input_allocas = verify
        .lines()
        .filter(|line| line.contains("json.input.i") && line.contains("alloca"))
        .count();
    assert_eq!(
        input_allocas, 1,
        "one loop formatting site needs one reusable input cell:\n{verify}"
    );
    let input_alloca = verify
        .find("json.input.i")
        .expect("JSON input cell in verifier");
    let first_branch = verify.find("\n  br ").expect("verifier entry branch");
    let format_call = verify
        .find("json.format.status")
        .expect("JSON format call in loop body");
    assert!(
        input_alloca < first_branch && first_branch < format_call,
        "the reusable JSON input cell must be allocated before entering the loop:\n{verify}"
    );
    for required in [
        "managed.root.reload",
        "loom_gc_typed_root_push_v1",
        TYPED_JSON_FORMAT_SYMBOL,
    ] {
        assert!(verify.contains(required), "missing `{required}`:\n{verify}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one gate keeps recursive Json carrier layout, exact tracing, forced relocation, backend differential behavior, and cross-target emission together"
)]
fn recursive_json_uses_one_exact_managed_cell_and_survives_forced_relocation() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-json/main.loom"),
        include_str!("../../../fixtures/lcir-typed-json/main_test.loom"),
    );

    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:?}");
    assert_eq!(interpreted[0].status, TestStatus::Passed, "{interpreted:?}");

    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native = emit_and_run_lcir(&artifact, "source-recursive-json");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(
        String::from_utf8_lossy(&native.output.stdout).contains("typedJson"),
        "{:?}",
        native.output
    );
    for required in [
        "loom_gc_typed_repeated_alloc_v1",
        "loom.lcir.list.descriptor",
        "loom.lcir.text_map.descriptor",
        "loom.lcir.list.pointer_offsets",
        "loom.lcir.text_map.pointer_offsets",
        "managed.root.rebuild.active.sum",
    ] {
        assert!(
            native.ir.contains(required),
            "missing `{required}`:\n{}",
            native.ir
        );
    }
    for forbidden in [
        "%loom.Value",
        "loom_runtime_text_map_get",
        "loom_runtime_text_map_insert",
        "ValueNode",
        "loom_executor_",
        "loom_gc_root_push_v1",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "recursive Json IR exposed `{forbidden}`:\n{}",
            native.ir
        );
    }

    let json_list_offsets = native
        .ir
        .lines()
        .find(|line| line.contains("@loom.lcir.list.pointer_offsets") && line.contains("[i64 16]"))
        .unwrap_or_else(|| panic!("Json List must trace only carrier byte 16:\n{}", native.ir));
    let json_list_suffix = json_list_offsets
        .split_once(" = ")
        .expect("Json List offsets global")
        .1;
    assert_eq!(
        json_list_suffix,
        "private unnamed_addr constant [1 x i64] [i64 16]"
    );
    assert!(native.ir.lines().any(|line| {
        line.contains("@loom.lcir.list.descriptor")
            && line.contains("i64 16, i64 8")
            && line.contains("i64 24, i64 1")
    }));
    assert!(native.ir.lines().any(|line| {
        line.contains("@loom.lcir.text_map.pointer_offsets")
            && line.contains("[2 x i64] [i64 0, i64 24]")
    }));
    assert!(native.ir.lines().any(|line| {
        line.contains("@loom.lcir.text_map.descriptor")
            && line.contains("i64 8, i64 8")
            && line.contains("i64 32, i64 2")
    }));

    let checked_mir = emit_and_run_checked_mir_tests(&program, "checked-mir-recursive-json");
    assert_eq!(checked_mir.status.success(), native.output.status.success());
    assert_eq!(checked_mir.stdout, native.output.stdout);
    assert_eq!(checked_mir.stderr, native.output.stderr);

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create recursive Json target directory");
        let object = directory.path().join("recursive-json.o");
        let ir_path = directory.path().join("recursive-json.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit recursive Json object for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read recursive Json target IR");
        assert!(
            ir.lines().any(|line| {
                line.contains("@loom.lcir.list.pointer_offsets") && line.contains("[i64 16]")
            }),
            "{target} lost the exact Json List pointer cell:\n{ir}"
        );
        assert!(
            ir.lines().any(|line| {
                line.contains("@loom.lcir.text_map.pointer_offsets")
                    && line.contains("[2 x i64] [i64 0, i64 24]")
            }),
            "{target} lost the exact Json TextMap pointer cells:\n{ir}"
        );
        assert!(!ir.contains("%loom.Value"), "{ir}");
    }
}

#[test]
fn recursive_json_is_typed_on_64_bit_and_fails_closed_on_32_bit() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-json/main.loom"),
        include_str!("../../../fixtures/lcir-typed-json/main_test.loom"),
    );
    let request = SourceArtifactRequest::Run {
        entry: "main".into(),
    };
    let artifact = lower_source_artifact_with_layout(
        &program,
        &request,
        TargetLayout::new(64).expect("64-bit target"),
    );
    assert!(dump_program(artifact.program()).contains("text_map.construct"));
    match lower_typed_artifact(
        &program,
        &request,
        TargetLayout::new(32).expect("32-bit target"),
    )
    .expect("classify 32-bit recursive Json")
    {
        LoweringOutcome::Unsupported(report) => assert!(
            report.items().iter().any(|item| {
                matches!(
                    item.feature(),
                    UnsupportedFeature::ExpressionType | UnsupportedFeature::SignatureType
                )
            }),
            "{report:?}"
        ),
        LoweringOutcome::Complete(_) => panic!("32-bit recursive Json must fail closed"),
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one gate proves compact collision-free Choice/Interleaved/Outer sum bytes, exact repeated tracing, forced relocation, differential behavior, and cross-target emission together"
)]
fn closed_sum_byte_classes_never_alias_scalar_bytes_with_managed_pointer_cells() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-sum-layout-collisions/main.loom"),
        include_str!("../../../fixtures/lcir-sum-layout-collisions/main_test.loom"),
    );

    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:?}");
    assert_eq!(interpreted[0].status, TestStatus::Passed, "{interpreted:?}");

    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native = emit_and_run_lcir(&artifact, "source-sum-layout-collisions");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(
        String::from_utf8_lossy(&native.output.stdout).contains("sumLayoutCollisions"),
        "{:?}",
        native.output
    );
    for required in [
        "loom_gc_typed_repeated_alloc_v1",
        "loom.lcir.list.pointer_offsets",
        "loom.lcir.text_map.pointer_offsets",
        "managed.root.rebuild.active.sum",
    ] {
        assert!(
            native.ir.contains(required),
            "missing `{required}`:\n{}",
            native.ir
        );
    }
    for forbidden in [
        "%loom.Value",
        "loom_runtime_text_map_get",
        "loom_runtime_text_map_insert",
        "ValueNode",
        "loom_executor_",
        "loom_gc_root_push_v1",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "collision-free sum IR exposed `{forbidden}`:\n{}",
            native.ir
        );
    }

    // Choice and Json both retain compact 24-byte values with one managed
    // cell at byte 16. The two Interleaved variants have opposing pointer and
    // scalar cells, so they must occupy offsets 0 and 8: the physical sum is
    // 32 bytes with exact managed cells at bytes 8 and 24. Outer nests Json
    // beside a 24-byte scalar tuple and grows only to 40 bytes, moving its one
    // managed cell to byte 32.
    for required in [
        "[1 x i64] [i64 16]",
        "i64 24, i64 1, ptr @loom.lcir.list.pointer_offsets",
        "[2 x i64] [i64 0, i64 24]",
        "i64 32, i64 2, ptr @loom.lcir.text_map.pointer_offsets",
        "[2 x i64] [i64 8, i64 24]",
        "i64 32, i64 2, ptr @loom.lcir.list.pointer_offsets",
        "[3 x i64] [i64 0, i64 16, i64 32]",
        "i64 40, i64 3, ptr @loom.lcir.text_map.pointer_offsets",
        "[1 x i64] [i64 32]",
        "i64 40, i64 1, ptr @loom.lcir.list.pointer_offsets",
        "[2 x i64] [i64 0, i64 40]",
        "i64 48, i64 2, ptr @loom.lcir.text_map.pointer_offsets",
    ] {
        assert!(
            native.ir.contains(required),
            "sum collision layout omitted `{required}`:\n{}",
            native.ir
        );
    }

    let checked_mir = emit_and_run_checked_mir_tests(&program, "checked-mir-sum-layout-collisions");
    assert_eq!(checked_mir.status.success(), native.output.status.success());
    assert_eq!(checked_mir.stdout, native.output.stdout);
    assert_eq!(checked_mir.stderr, native.output.stderr);

    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let directory = tempfile::tempdir().expect("create sum-layout target directory");
        let object = directory.path().join("sum-layout-collisions.o");
        let ir_path = directory.path().join("sum-layout-collisions.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit sum-layout object for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read sum-layout target IR");
        for required in [
            "[1 x i64] [i64 16]",
            "[2 x i64] [i64 0, i64 24]",
            "[2 x i64] [i64 8, i64 24]",
            "[3 x i64] [i64 0, i64 16, i64 32]",
            "[1 x i64] [i64 32]",
            "[2 x i64] [i64 0, i64 40]",
        ] {
            assert!(
                ir.contains(required),
                "{target} omitted `{required}`:\n{ir}"
            );
        }
        assert!(!ir.contains("%loom.Value"), "{ir}");
    }

    let request = SourceArtifactRequest::Run {
        entry: "main".into(),
    };
    match lower_typed_artifact(
        &program,
        &request,
        TargetLayout::new(32).expect("32-bit target"),
    )
    .expect("classify 32-bit managed sums")
    {
        LoweringOutcome::Unsupported(report) => assert!(
            report.items().iter().any(|item| {
                matches!(
                    item.feature(),
                    UnsupportedFeature::ExpressionType | UnsupportedFeature::SignatureType
                )
            }),
            "{report:?}"
        ),
        LoweringOutcome::Complete(_) => panic!("32-bit managed sum graph must fail closed"),
    }
}

#[test]
fn deep_nested_sum_layout_is_cached_and_bounded_across_the_complete_graph() {
    const LAYERS: usize = 24;
    let mut source = String::from("enum Layer0 { Number(Int), Label(Text) }\n");
    for layer in 1..LAYERS {
        writeln!(
            source,
            "enum Layer{layer} {{ Nested(Layer{}), Scalars((Int, Int, Int)) }}",
            layer - 1
        )
        .expect("append nested sum declaration");
    }
    source.push_str(
        "\nfn join(left Text, right Text) Text { left.concat(right) }\n\nfn label0(value Layer0) Text {\n    match value {\n        Label(text) => text\n        _ => \"missing\"\n    }\n}\n",
    );
    for layer in 1..LAYERS {
        writeln!(
            source,
            "\nfn label{layer}(value Layer{layer}) Text {{\n    match value {{\n        Nested(inner) => label{}(inner)\n        _ => \"missing\"\n    }}\n}}",
            layer - 1
        )
        .expect("append nested sum matcher");
    }
    let mut wrapped = "Layer0.Label(kept)".to_owned();
    for layer in 1..LAYERS {
        wrapped = format!("Layer{layer}.Nested({wrapped})");
    }
    writeln!(
        source,
        "\npub fn main() {{\n    let kept = join(\"de\", \"ep\")\n    let values = [{wrapped}]\n    let pressure = join(\"mo\", \"ved\")\n    let valid = match values.get(0) {{\n        Some(value) => label{}(value) == \"deep\",\n        None => false,\n    }}\n    assert valid\n    discard pressure\n}}",
        LAYERS - 1
    )
    .expect("append nested sum entry");

    let program = compile_source(&source);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let directory = tempfile::tempdir().expect("create deep sum-layout directory");
    let object = directory.path().join("deep-sum-layout.o");
    let ir_path = directory.path().join("deep-sum-layout.ll");
    emit_lcir_native_object(
        &artifact,
        &object,
        &NativeObjectOptions {
            emit_ir: Some(ir_path.clone()),
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit a deeply nested sum graph within the shared layout budget");
    assert!(object.is_file());
    let ir = std::fs::read_to_string(ir_path).expect("read deep sum-layout IR");
    assert!(ir.contains("loom.lcir.list.pointer_offsets"), "{ir}");
    assert!(!ir.contains("%loom.Value"), "{ir}");
}

fn wide_sum_definition(name: &str, scalar_fields: usize) -> String {
    let fields = std::iter::repeat_n("Int", scalar_fields)
        .collect::<Vec<_>>()
        .join(", ");
    format!("enum {name} {{ Scalars({fields}), Managed(Text) }}\n")
}

fn wide_sum_construct(name: &str, scalar_fields: usize) -> String {
    let fields = std::iter::repeat_n("0", scalar_fields)
        .collect::<Vec<_>>()
        .join(", ");
    format!("    discard {name}.Scalars({fields})\n")
}

fn assert_sum_emission_resource_error(source: &str, label: &str, message: &str) {
    let program = compile_source(source);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let directory = tempfile::tempdir().expect("create bounded sum-emission directory");
    let object = directory.path().join(format!("{label}.o"));
    let ir_path = directory.path().join(format!("{label}.ll"));
    let error = emit_lcir_native_object(
        &artifact,
        &object,
        &NativeObjectOptions {
            emit_ir: Some(ir_path.clone()),
            ..NativeObjectOptions::default()
        },
    )
    .expect_err("shared sum resource exhaustion must fail before LLVM IR materialization");
    assert_eq!(error.code(), "ProgramTooLarge", "{error}");
    assert!(
        error.message().contains(message),
        "unexpected structured resource error: {error}"
    );
    assert!(
        !object.exists(),
        "resource rejection emitted a partial object"
    );
    assert!(!ir_path.exists(), "resource rejection emitted bytewise IR");
}

#[test]
fn independent_wide_sums_share_one_bounded_carrier_placement_budget() {
    const SCALAR_FIELDS: usize = 250;
    const SUMS: usize = 11;
    let mut source = String::new();
    for index in 0..SUMS {
        source.push_str(&wide_sum_definition(&format!("Wide{index}"), SCALAR_FIELDS));
    }
    source.push_str("\npub fn main() {\n");
    for index in 0..SUMS {
        source.push_str(&wide_sum_construct(&format!("Wide{index}"), SCALAR_FIELDS));
    }
    source.push_str("}\n");

    assert_sum_emission_resource_error(&source, "shared-sum-placement", "sum carrier placement");
}

#[test]
fn repeated_wide_sum_packing_shares_one_bounded_ir_emission_budget() {
    const SCALAR_FIELDS: usize = 250;
    const CONSTRUCTS: usize = 34;
    let mut source = String::new();
    source.push_str(&wide_sum_definition("Wide", SCALAR_FIELDS));
    source.push_str("\npub fn main() {\n");
    for _ in 0..CONSTRUCTS {
        source.push_str(&wide_sum_construct("Wide", SCALAR_FIELDS));
    }
    source.push_str("}\n");

    assert_sum_emission_resource_error(&source, "shared-sum-emission", "sum carrier pack/unpack");
}

#[test]
fn typed_text_map_source_is_direct_on_64_bit_and_fails_closed_on_32_bit() {
    let program = compile_source(
        "pub fn main() {\n    let values = TextMap[Int]().insert(\"answer\", 42)\n    let same = TextMap[Int]().insert(\"answer\", 42)\n    discard values.contains(\"answer\")\n    discard values.remove(\"missing\")\n    discard values.get(\"answer\")\n    discard values == same\n}\n",
    );
    let request = SourceArtifactRequest::Run {
        entry: "main".into(),
    };
    let artifact = lower_source_artifact_with_layout(
        &program,
        &request,
        TargetLayout::new(64).expect("64-bit target"),
    );
    let dump = dump_program(artifact.program());
    for required in [
        "text_map.insert",
        "text_map.contains",
        "text_map.remove",
        "text_map.entry_get",
    ] {
        assert!(
            dump.contains(required),
            "64-bit TextMap[Int] omitted `{required}`:\n{dump}"
        );
    }
    match lower_typed_artifact(
        &program,
        &request,
        TargetLayout::new(32).expect("32-bit target"),
    )
    .expect("classify 32-bit TextMap")
    {
        LoweringOutcome::Unsupported(report) => assert!(
            report.items().iter().any(|item| {
                matches!(
                    item.feature(),
                    UnsupportedFeature::ExpressionType | UnsupportedFeature::SignatureType
                )
            }),
            "{report:?}"
        ),
        LoweringOutcome::Complete(_) => panic!("32-bit managed TextMap must fail closed"),
    }
}

fn generic_instance_plan(
    program: &CheckedProgram,
    artifact: &CheckedArtifact,
) -> (BTreeMap<Type, InstanceId>, InstanceId) {
    let source_function = |name: &str| {
        program
            .as_program()
            .functions
            .iter()
            .find(|function| function.name == name)
            .map_or_else(
                || panic!("missing checked MIR function `{name}`"),
                |function| function.id,
            )
    };
    let identity_source = source_function("standalone.identity");
    let preserve_source = source_function("standalone.preserve");
    let dump = dump_program(artifact.program());
    let mut identity_instances = BTreeMap::new();
    let mut preserve_instance = None;
    for function in artifact.functions() {
        let key = artifact
            .program()
            .as_program()
            .instance_key(function.id())
            .expect("LCIR function instance key");
        if key.source() == identity_source {
            assert_eq!(key.role(), InstanceRole::AssumedBody, "{dump}");
            let [ty] = key.type_arguments() else {
                panic!("identity requires one exact type argument: {key}")
            };
            assert!(key.witness_arguments().is_empty(), "{key}");
            assert!(
                identity_instances
                    .insert(ty.clone(), function.id())
                    .is_none(),
                "duplicate identity instance for {ty:?}: {dump}"
            );
        } else if key.source() == preserve_source {
            assert_eq!(key.role(), InstanceRole::AssumedBody, "{dump}");
            assert_eq!(key.type_arguments(), &[Type::Int], "{key}");
            assert!(
                matches!(
                    key.witness_arguments(),
                    [InstanceWitnessArgument::Concrete(_)]
                ),
                "{key}"
            );
            assert!(
                preserve_instance.replace(function.id()).is_none(),
                "duplicate preserve instance: {dump}"
            );
        }
    }
    assert_eq!(
        identity_instances.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([Type::Bool, Type::Float, Type::Int]),
        "{dump}"
    );
    (
        identity_instances,
        preserve_instance.expect("reachable preserve[Int: Marker] instance"),
    )
}

#[test]
fn generic_instances_use_direct_host_and_msvc_target_abis() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-generics/main.loom"),
        include_str!("../../../fixtures/lcir-generics/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let (identity_instances, preserve_instance) = generic_instance_plan(&program, &artifact);

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
    for (ty, abi) in [
        (Type::Bool, "i1"),
        (Type::Float, "double"),
        (Type::Int, "i64"),
    ] {
        let instance = identity_instances
            .get(&ty)
            .copied()
            .expect("checked identity instance");
        let signature = format!(
            "define internal {abi} @loom.lcir.fn.{}({abi}",
            instance.raw()
        );
        assert!(
            native.ir.contains(&signature),
            "missing `{signature}`:\n{}",
            native.ir
        );
    }
    let preserve_signature = format!(
        "define internal i64 @loom.lcir.fn.{}(i64",
        preserve_instance.raw()
    );
    assert!(
        native.ir.contains(&preserve_signature),
        "missing `{preserve_signature}`:\n{}",
        native.ir
    );
    assert_pure_surface(&native.ir);

    let checked_mir = emit_and_run_checked_mir(&program, "main", "source-generics-checked-mir");
    assert_eq!(checked_mir.status.success(), native.output.status.success());
    assert_eq!(checked_mir.stdout, native.output.stdout);
    assert_eq!(checked_mir.stderr, native.output.stderr);

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
        msvc_ir.contains(&format!(
            "define internal i64 @loom.lcir.fn.{}(i64",
            identity_instances[&Type::Int].raw()
        )),
        "{msvc_ir}"
    );
    assert_pure_surface(&msvc_ir);
}

#[test]
fn generic_products_and_proven_wrappers_execute_through_typed_lcir() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-generic-products/main.loom"),
        include_str!("../../../fixtures/lcir-generic-products/main_test.loom"),
    );
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-generic-products");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
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
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-structural-equality/main.loom"),
        include_str!("../../../fixtures/lcir-structural-equality/main_test.loom"),
    );
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-structural-equality");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
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

#[test]
fn recursive_structural_equality_executes_typed_helper_cycles_without_runtime_type_dispatch() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-recursive-equality/main.loom"),
        include_str!("../../../fixtures/lcir-recursive-equality/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let interpreted_tests = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted_tests.len(), 1, "{interpreted_tests:#?}");
    assert_eq!(interpreted_tests[0].status, TestStatus::Passed);

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let helpers = artifact
        .functions()
        .iter()
        .filter(|function| {
            artifact
                .program()
                .as_program()
                .instance_key(function.id())
                .is_some_and(|key| key.role() == InstanceRole::StructuralEquality)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        helpers.len(),
        9,
        "Node/List and complete Json/List/TextMap shapes must each share one exact helper:\n{}",
        dump_program(artifact.program())
    );
    assert!(
        helpers
            .iter()
            .all(|helper| helper.effects() == Effects::NONE)
    );
    let helper_ids = helpers
        .iter()
        .map(|helper| helper.id())
        .collect::<BTreeSet<_>>();
    let helper_calls = helpers
        .iter()
        .flat_map(|helper| helper.instructions())
        .filter_map(|instruction| match instruction.kind() {
            InstructionKind::DirectCall { callee, .. } if helper_ids.contains(callee) => {
                Some(*callee)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        helper_calls.len() >= helpers.len(),
        "{}",
        dump_program(artifact.program())
    );

    let native = emit_and_run_lcir_with_options(
        &artifact,
        "source-recursive-structural-equality",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );
    let checked_mir = emit_and_run_checked_mir(
        &program,
        "main",
        "checked-mir-recursive-structural-equality",
    );
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
    for forbidden in [
        "%loom.Value",
        "@loom.fn.",
        "loom_runtime_json_equal",
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
    assert_no_indirect_calls(&native.ir);

    let tests = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native_tests = emit_and_run_lcir(&tests, "source-recursive-structural-equality-tests");
    let checked_mir_tests =
        emit_and_run_checked_mir_tests(&program, "checked-mir-recursive-structural-equality-tests");
    assert!(
        native_tests.output.status.success(),
        "{:?}",
        native_tests.output
    );
    assert!(checked_mir_tests.status.success(), "{checked_mir_tests:?}");
    assert_eq!(native_tests.output.stdout, checked_mir_tests.stdout);
    assert_eq!(native_tests.output.stderr, checked_mir_tests.stderr);
}

fn static_concepts_test_artifact() -> CheckedArtifact {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-static-concepts/main.loom"),
        include_str!("../../../fixtures/lcir-static-concepts/main_test.loom"),
    );
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].name, "standalone.staticConcepts",
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
        String::from_utf8_lossy(&native.output.stdout).contains("passed standalone.staticConcepts"),
        "{:?}",
        native.output
    );
    assert_stateless_direct_lcir_surface(&native.ir);
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
    assert_stateless_direct_lcir_surface(&ir);
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
fn concepts_polymorphism_main_devirtualizes_unique_dynamic_witnesses_to_direct_calls() {
    let source = include_str!("../../../examples/concepts-polymorphism/concepts.loom");
    let program = compile_source(source);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    for expected in [
        "standalone.label",
        "standalone.format",
        "standalone.next",
        "standalone.static_label",
        "standalone.dynamic_format",
        "standalone.take_one",
    ] {
        assert!(dump.contains(expected), "missing `{expected}`:\n{dump}");
    }
    for dead in [
        "standalone.erase_label",
        "standalone.forward_label",
        "standalone.erase_source",
    ] {
        assert!(!dump.contains(dead), "retained dead `{dead}`:\n{dump}");
    }
    assert!(
        !dump.contains("View["),
        "dyn representation leaked:\n{dump}"
    );

    let native = emit_and_run_lcir(&artifact, "source-concepts-unique-dyn");
    let checked_mir = emit_and_run_checked_mir(&program, "main", "mir-concepts-unique-dyn");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
    assert_stateless_direct_lcir_surface(&native.ir);
    assert_no_indirect_calls(&native.ir);
    for forbidden in ["loom_witness_", "WitnessInstance", "loom_executor_"] {
        assert!(
            !native.ir.contains(forbidden),
            "unexpected `{forbidden}`:\n{}",
            native.ir
        );
    }
}

#[test]
fn closed_conditional_dynamic_proofs_use_finite_direct_lcir_dispatch() {
    let source = r"dyn concept Measure { method measure(self) Int }

record One {}
record Two {}
record Boxed[T] { value T }

impl Measure for One {
    method measure(self) Int { 1 }
}

impl Measure for Two {
    method measure(self) Int { 2 }
}

impl[T: Measure] Measure for Boxed[T] {
    method measure(self) Int { self.value.measure() }
}

fn choose(first Bool) dyn Measure {
    if first {
        Boxed { value = One {} }
    } else {
        Boxed { value = Two {} }
    }
}

pub fn main() {
    let one = choose(true).measure()
    let two = choose(false).measure()
    assert one == 1
    assert two == 2
}
";
    let program = compile_source(source);
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::LcirOnly,
    )
    .expect("prepare closed conditional dynamic program");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let [dynamic] = artifact.representations().dynamics() else {
        panic!("Boxed[One] and Boxed[Two] must form one finite catalog")
    };
    assert_eq!(dynamic.candidates().len(), 2);

    let native = emit_and_run_lcir(&artifact, "source-closed-conditional-dyn");
    let checked_mir = emit_and_run_checked_mir(&program, "main", "mir-closed-conditional-dyn");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
    assert!(native.ir.contains("switch i32"), "{}", native.ir);
    for forbidden in [
        "%loom.Value",
        "ValueNode",
        "loom_witness_",
        "WitnessInstance",
        "dyn.registry",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "closed conditional dyn IR exposed `{forbidden}`:\n{}",
            native.ir
        );
    }
    assert_no_indirect_calls(&native.ir);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one Core02 gate keeps nested dyn erasure, precise repeated descriptors, copy independence, forced relocation, and cross-target objects together"
)]
fn concepts_polymorphism_tests_erase_dynamic_storage_to_concrete_layouts() {
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
    let source = format!(
        r"{}

record DynWide {{
    item dyn Labeled
{fields}
}}

fn joinDynStorage(left Text, right Text) Text {{ left.concat(right) }}
",
        include_str!("../../../examples/concepts-polymorphism/concepts.loom")
    );
    let test_source = format!(
        r#"{}

test fn dynamicStorageGcPressure() {{
    let raw = Label {{ text = joinDynStorage("Rel", "ocated") }}
    let erased = erase_label(raw)
    let wide = DynWide {{ item = erased, {initializers} }}
    var values = [{repeated}]
    let alias = values
    values.add(wide)
    let trigger = [{repeated}]
    let triggerLength = trigger.length()
    let valuesLength = values.length()
    let aliasLength = alias.length()
    assert triggerLength == 129
    assert valuesLength == 130
    assert aliasLength == 129
    assert raw.text == "Relocated"
    match values.get(129) {{
        Some(item) => {{
            let text = item.item.label()
            assert text == "Relocated"
            Unit
        }}
        None => {{
            assert false
            Unit
        }}
    }}
    match alias.get(0) {{
        Some(item) => {{
            let text = item.item.label()
            assert text == "Relocated"
            Unit
        }}
        None => {{
            assert false
            Unit
        }}
    }}
    let interfaces = [erased, erase_label(Label {{ text = "other" }})]
    match interfaces.get(0) {{
        Some(value) => {{
            let text = value.label()
            assert text == "Relocated"
            Unit
        }}
        None => {{
            assert false
            Unit
        }}
    }}
}}
"#,
        include_str!("../../../examples/concepts-polymorphism/concepts_test.loom")
    );
    let program = compile_sources(&source, &test_source);
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 4, "{interpreted:#?}");
    assert!(
        interpreted
            .iter()
            .all(|test| test.status == TestStatus::Passed),
        "{interpreted:#?}"
    );

    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(artifact.program());
    for required in [
        "standalone.erase_label",
        "standalone.forward_label",
        "standalone.erase_source",
        "standalone.dynamicStorageGcPressure",
        "list.construct",
        "list.append",
        "list.get",
        "sum.switch",
        "inout=[0]",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    assert!(
        !dump.contains("View["),
        "dynamic storage leaked into LCIR:\n{dump}"
    );

    let label_id = program
        .types
        .iter()
        .find(|definition| definition.name == "Label")
        .map(|definition| definition.id)
        .expect("Core02 Label type");
    let holder_id = program
        .types
        .iter()
        .find(|definition| definition.name == "LabelHolder")
        .map(|definition| definition.id)
        .expect("Core02 LabelHolder type");
    let packet_id = program
        .types
        .iter()
        .find(|definition| definition.name == "LabelPacket")
        .map(|definition| definition.id)
        .expect("Core02 LabelPacket type");
    let labeled = program
        .concepts
        .iter()
        .find(|concept| concept.name == "Labeled")
        .map(|concept| concept.id)
        .expect("Core02 Labeled concept");
    let concrete = Type::Nominal(label_id, Vec::new());
    let view = Type::View {
        mutable: false,
        concept: labeled,
        bindings: BTreeMap::new(),
    };
    let physical_list = Type::List(Box::new(concrete.clone()));
    let source_list = Type::List(Box::new(view.clone()));
    let representations = artifact.representations();
    let concrete_type = representations
        .type_id(&concrete)
        .expect("one concrete Label representation");
    assert_eq!(
        representations
            .value_types()
            .iter()
            .filter(|ty| ty.semantic() == &concrete)
            .count(),
        1,
        "raw and erased Label values must share one physical value type"
    );
    assert!(representations.type_id(&view).is_none());
    assert!(representations.type_id(&source_list).is_none());
    assert!(representations.type_id(&physical_list).is_some());

    let holder = representations
        .type_id(&Type::Nominal(holder_id, Vec::new()))
        .expect("LabelHolder representation");
    let Repr::Product(holder_product) = representations
        .value_type(holder)
        .and_then(|ty| representations.repr(ty.repr()))
        .copied()
        .expect("LabelHolder physical representation")
    else {
        panic!("LabelHolder must remain an unboxed product")
    };
    assert_eq!(
        representations
            .product(holder_product)
            .expect("LabelHolder product")
            .fields(),
        [concrete_type]
    );
    let packet = representations
        .type_id(&Type::Nominal(packet_id, Vec::new()))
        .expect("LabelPacket representation");
    let Repr::Sum(packet_sum) = representations
        .value_type(packet)
        .and_then(|ty| representations.repr(ty.repr()))
        .copied()
        .expect("LabelPacket physical representation")
    else {
        panic!("LabelPacket must remain a closed sum")
    };
    assert_eq!(
        representations
            .sum(packet_sum)
            .expect("LabelPacket sum")
            .variants()[0]
            .fields(),
        [concrete_type]
    );

    let native = emit_and_run_lcir(&artifact, "source-concepts-dyn-storage");
    let checked_mir = emit_and_run_checked_mir_tests(&program, "mir-concepts-dyn-storage");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
    for required in [
        "loom_gc_typed_repeated_alloc_v1",
        "loom.lcir.list.descriptor",
        "loom.lcir.list.pointer_offsets",
        "managed.root.reload",
    ] {
        assert!(
            native.ir.contains(required),
            "dynamic storage IR omitted `{required}`:\n{}",
            native.ir
        );
    }
    for forbidden in [
        "%loom.Value",
        "ValueNode",
        "loom_executor_",
        "loom_witness_",
        "WitnessInstance",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "dynamic storage IR exposed `{forbidden}`:\n{}",
            native.ir
        );
    }
    assert_no_indirect_calls(&native.ir);

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create dyn-storage target output");
        let object = directory.path().join(if target.contains("windows") {
            "concepts-dyn-storage.obj"
        } else {
            "concepts-dyn-storage.o"
        });
        let ir_path = directory.path().join("concepts-dyn-storage.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                emit_ir: Some(ir_path.clone()),
                target_triple: Some(target.to_owned()),
                optimization: OptimizationProfile::Release,
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit dyn-storage object for {target}: {error}"));
        let bytes = std::fs::read(&object).expect("read dyn-storage object");
        if target.contains("windows") {
            assert_eq!(bytes.get(..2), Some([0x64, 0x86].as_slice()));
        } else {
            assert_eq!(bytes.get(..4), Some([0x7f, b'E', b'L', b'F'].as_slice()));
        }
        let ir = std::fs::read_to_string(ir_path).expect("read dyn-storage target IR");
        assert!(ir.contains("loom.lcir.list.descriptor"), "{target}: {ir}");
        assert!(
            ir.contains("loom.lcir.list.pointer_offsets"),
            "{target}: {ir}"
        );
        assert!(!ir.contains("loom_witness_"), "{target}: {ir}");
        assert_no_indirect_calls(&ir);
    }
}

#[test]
fn unique_dynamic_witness_dce_ignores_dead_conformances_and_method_slots() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-dyn-unique/main.loom"),
        include_str!("../../../fixtures/lcir-dyn-unique/main_test.loom"),
    );
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(artifact.program());
    assert!(dump.contains("standalone.read"), "{dump}");
    assert!(dump.contains("standalone.measure"), "{dump}");
    assert!(
        dump.matches("inout=[0]").count() >= 2,
        "mutable interface and concrete method must both retain writeback:\n{dump}"
    );
    for dead in ["standalone.cold", "UnusedCounter", "9001", "9002"] {
        assert!(!dump.contains(dead), "retained dead `{dead}`:\n{dump}");
    }

    let native = emit_and_run_lcir(&artifact, "source-unique-dyn-dce");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(
        String::from_utf8_lossy(&native.output.stdout)
            .contains("passed standalone.uniqueDynamicWitness"),
        "{:?}",
        native.output
    );
    assert_stateless_direct_lcir_surface(&native.ir);
    assert_no_indirect_calls(&native.ir);
    for forbidden in ["standalone.cold", "UnusedCounter", "loom_witness_"] {
        assert!(
            !native.ir.contains(forbidden),
            "unexpected `{forbidden}`:\n{}",
            native.ir
        );
    }

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create dyn target output");
        let object = directory.path().join(if target.contains("windows") {
            "unique-dyn.obj"
        } else {
            "unique-dyn.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                optimization: OptimizationProfile::Release,
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit unique dyn object for {target}: {error}"));
        assert!(object.is_file(), "missing object for {target}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one finite-dyn gate keeps checked representation, DCE, moving-GC value semantics, differential execution, and cross-target objects together"
)]
fn finite_dynamic_witnesses_use_precise_single_pointer_boxes_and_direct_dispatch() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-dyn-finite/main.loom"),
        include_str!("../../../fixtures/lcir-dyn-finite/main_test.loom"),
    );
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    let prepared =
        prepare_native_object(&program, EmitOptions::tests(), NativeRoutePolicy::LcirOnly)
            .expect("prepare finite dynamic tests");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let representations = artifact.representations();
    assert_eq!(representations.dynamics().len(), 2);
    for dynamic in representations.dynamics() {
        assert_eq!(dynamic.candidates().len(), 2);
        assert_eq!(
            representations
                .value_type(dynamic.view())
                .and_then(|ty| representations.repr(ty.repr())),
            Some(&Repr::ManagedPointer)
        );
    }
    let dynamic_list = representations
        .registrations()
        .iter()
        .find(|registration| {
            matches!(
                registration.semantic(),
                Type::List(element) if matches!(element.as_ref(), Type::View { .. })
            )
        })
        .expect("List[dyn Metric] representation");
    assert_eq!(
        representations
            .value_type(dynamic_list.value_type())
            .and_then(|ty| representations.repr(ty.repr())),
        Some(&Repr::ManagedPointer)
    );

    let dump = dump_program(artifact.program());
    for required in [
        "dynamic d",
        "dyn.construct",
        "dyn.switch",
        "list.construct",
        "list.get",
        "inout=[0]",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    for dead in ["standalone.cold", "7001", "7002"] {
        assert!(!dump.contains(dead), "retained dead `{dead}`:\n{dump}");
    }
    let verify = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("verify"))
        .expect("finite-dyn verification function");
    let checked_stores = artifact
        .functions()
        .iter()
        .filter(|function| function.name().ends_with("storeChecked"))
        .map(loom_codegen_ir::Function::id)
        .collect::<BTreeSet<_>>();
    assert_eq!(checked_stores.len(), 2, "{dump}");
    assert!(checked_stores.iter().all(|callee| {
        artifact
            .function(*callee)
            .is_some_and(|function| function.effects().contains(Effects::MAY_FAULT))
    }));
    let invoked_checked_stores = verify
        .blocks()
        .iter()
        .filter_map(
            |block| match block.terminator().map(loom_codegen_ir::Terminator::kind) {
                Some(loom_codegen_ir::TerminatorKind::Invoke { callee, .. })
                    if checked_stores.contains(callee) =>
                {
                    Some(*callee)
                }
                _ => None,
            },
        )
        .collect::<BTreeSet<_>>();
    assert_eq!(
        invoked_checked_stores, checked_stores,
        "both candidates of storeChecked must use fallible Invoke edges"
    );
    let projected_dispatch = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("advanceStored"))
        .expect("projected finite-dyn dispatch method");
    assert_eq!(projected_dispatch.signature().inout_params(), [0]);
    assert!(projected_dispatch.blocks().iter().any(|block| matches!(
        block.terminator().map(loom_codegen_ir::Terminator::kind),
        Some(loom_codegen_ir::TerminatorKind::DynSwitch { cases, .. }) if cases.len() == 2
    )));
    assert_eq!(
        projected_dispatch
            .instructions()
            .iter()
            .filter(|instruction| matches!(
                instruction.kind(),
                InstructionKind::DynConstruct { .. }
            ))
            .count(),
        4,
        "two candidates need normal and fault-edge fresh boxes"
    );
    assert_eq!(
        projected_dispatch
            .instructions()
            .iter()
            .filter(|instruction| matches!(
                instruction.kind(),
                InstructionKind::ProductInsert { .. }
            ))
            .count(),
        8,
        "each fresh box must reconstruct the slot and holder on both exits"
    );

    let native = emit_and_run_lcir(&artifact, "source-finite-dyn");
    let checked_mir = emit_and_run_checked_mir_tests(&program, "checked-mir-finite-dyn");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(native.output.stdout, checked_mir.stdout);
    assert_eq!(native.output.stderr, checked_mir.stderr);
    assert!(
        String::from_utf8_lossy(&native.output.stdout)
            .contains("passed standalone.finiteDynamicWitnesses"),
        "{:?}",
        native.output
    );
    let descriptors = native
        .ir
        .lines()
        .filter(|line| line.starts_with("@loom.lcir.dyn.descriptor."))
        .collect::<Vec<_>>();
    assert_eq!(descriptors.len(), 4, "{}", native.ir);
    assert!(
        descriptors
            .iter()
            .any(|line| line.contains("i64 1, ptr @loom.lcir.dyn.pointer_offsets.")),
        "Counter candidates need one exact managed Text offset:\n{}",
        native.ir
    );
    assert!(
        descriptors
            .iter()
            .any(|line| line.contains("i64 0, ptr null")),
        "Offset candidates must be pointer-free:\n{}",
        native.ir
    );
    for required in [
        "loom_gc_typed_alloc_v1",
        "loom.lcir.dyn.pointer_offsets.",
        "switch i32",
        "managed.root.reload",
    ] {
        assert!(
            native.ir.contains(required),
            "finite dyn IR omitted `{required}`:\n{}",
            native.ir
        );
    }
    for forbidden in [
        "%loom.Value",
        "ValueNode",
        "loom_executor_",
        "loom_witness_",
        "WitnessInstance",
        "dyn.registry",
        "standalone.cold",
    ] {
        assert!(
            !native.ir.contains(forbidden),
            "finite dyn IR exposed `{forbidden}`:\n{}",
            native.ir
        );
    }
    assert_no_indirect_calls(&native.ir);
    let projected_dispatch = emitted_lcir_instance(&native.ir, projected_dispatch);
    assert_eq!(
        projected_dispatch
            .matches("call i32 @loom_gc_typed_alloc_v1")
            .count(),
        4,
        "projected normal and fault exits must allocate fresh immutable boxes:\n{projected_dispatch}"
    );
    assert!(
        projected_dispatch.matches("product.extract").count() >= 2
            && projected_dispatch.matches("product.insert").count() >= 8,
        "projected dynamic dispatch must rebuild both product parents on normal and fault exits:\n{projected_dispatch}"
    );
    assert!(
        projected_dispatch.contains("dyn.switch.tag")
            && projected_dispatch.contains("dyn.construct.output"),
        "projected dispatch must remain a finite direct switch with fresh boxes:\n{projected_dispatch}"
    );
    let mutable_dispatch = native
        .ir
        .split("\ndefine ")
        .find(|body| {
            body.contains("dyn.switch.tag")
                && body.contains("dyn.construct.output")
                && body.contains("ret { i32, i64, ptr }")
        })
        .expect("mutable finite-dyn dispatch function");
    assert_eq!(
        mutable_dispatch
            .matches("call i32 @loom_gc_typed_alloc_v1")
            .count(),
        4,
        "both candidates need normal and fault-edge fresh boxes:\n{mutable_dispatch}"
    );
    assert!(
        mutable_dispatch.contains("insertvalue { i32, i64, ptr } { i32 0")
            && mutable_dispatch.contains("insertvalue { i32, i64, ptr } { i32 1"),
        "normal and fault exits must both return owner writeback:\n{mutable_dispatch}"
    );
    assert!(
        !mutable_dispatch.contains("store ptr %0, ptr %managed.root."),
        "the superseded dyn box must not remain a GC root while allocating its replacement:\n{mutable_dispatch}"
    );
    assert!(
        mutable_dispatch.contains("store ptr %managed.root.product.extract"),
        "only managed leaves of the updated concrete payload should be rooted:\n{mutable_dispatch}"
    );

    let fault_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "projectedFaultMain".into(),
        },
    );
    let interpreted_fault =
        Command::new(std::env::current_exe().expect("current LCIR test executable"))
            .args([
                "--exact",
                "finite_dynamic_projected_fault_interpreter_child",
                "--nocapture",
            ])
            .env(FINITE_DYN_FAULT_INTERPRETER_CHILD_ENV, "1")
            .output()
            .expect("run projected finite-dyn interpreter child");
    assert!(interpreted_fault.status.success(), "{interpreted_fault:?}");
    let interpreted_stdout = String::from_utf8_lossy(&interpreted_fault.stdout);
    assert!(
        interpreted_stdout.contains("projected fault writeback\n"),
        "{interpreted_fault:?}"
    );
    assert!(
        !interpreted_stdout.contains("stale projected fault writeback"),
        "{interpreted_fault:?}"
    );
    let faulted =
        emit_and_run_lcir_machine_fault(&fault_artifact, "finite-dyn-projected-writeback-fault");
    assert!(!faulted.output.status.success(), "{:?}", faulted.output);
    assert_eq!(
        faulted.output.stdout, b"projected fault writeback\n",
        "LCIR defer observed stale sibling state"
    );
    assert_eq!(
        machine_fault(&faulted.output)["fault"]["code"],
        "AssertionFault",
        "LCIR projected writeback changed the primary method fault"
    );
    for forbidden in ["%loom.Value", "ValueNode", "loom_witness_", "dyn.registry"] {
        assert!(
            !faulted.ir.contains(forbidden),
            "fault writeback exposed `{forbidden}`:\n{}",
            faulted.ir
        );
    }
    assert_no_indirect_calls(&faulted.ir);

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create finite-dyn target output");
        let object = directory.path().join(if target.contains("windows") {
            "finite-dyn.obj"
        } else {
            "finite-dyn.o"
        });
        let ir_path = directory.path().join("finite-dyn.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                emit_ir: Some(ir_path.clone()),
                target_triple: Some(target.to_owned()),
                optimization: OptimizationProfile::Development,
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit finite-dyn object for {target}: {error}"));
        let bytes = std::fs::read(&object).expect("read finite-dyn object");
        if target.contains("windows") {
            assert_eq!(bytes.get(..2), Some([0x64, 0x86].as_slice()));
        } else {
            assert_eq!(bytes.get(..4), Some([0x7f, b'E', b'L', b'F'].as_slice()));
        }
        let ir = std::fs::read_to_string(ir_path).expect("read finite-dyn target IR");
        assert!(ir.contains("loom.lcir.dyn.descriptor."), "{target}: {ir}");
        assert!(ir.contains("switch i32"), "{target}: {ir}");
        assert!(!ir.contains("loom_witness_"), "{target}: {ir}");
        assert_no_indirect_calls(&ir);
    }

    match lower_typed_artifact(
        &program,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(32).expect("32-bit target"),
    )
    .expect("classify 32-bit finite dyn")
    {
        LoweringOutcome::Unsupported(report) => assert!(
            report.items().iter().any(|item| matches!(
                item.feature(),
                UnsupportedFeature::ExpressionType | UnsupportedFeature::SignatureType
            )),
            "{report:?}"
        ),
        LoweringOutcome::Complete(_) => panic!("32-bit finite dyn must fail closed"),
    }
}

#[test]
fn source_ranges_emit_proved_nsw_successors_without_fault_abi() {
    let source = r"fn highBit() Int {
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

pub fn main() {
    discard highBit()
    discard nested(3, 4)
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
#[expect(
    clippy::too_many_lines,
    reason = "one differential fixture covers both loop forms, control edges, cleanup execution, and carried mutations"
)]
fn loop_control_and_cleanup_agree_across_interpreter_lcir_and_checked_mir_native() {
    let source = r"fn whileTotal() Int {
    var index = 0
    var total = 0
    while index < 5 {
        index = index + 1
        defer {
            total = total + 10
        }
        if index == 2 {
            continue
        } else {}
        total = total + 1
        if index == 4 {
            break
        } else {}
    }
    total
}

fn rangeTotal() Int {
    var total = 0
    for index in 0..5 {
        if index == 2 {
            continue
        } else {}
        total = total + index
    }
    total
}

fn conditionalWhileBreak(flag Bool) Int {
    var observed = 0
    while true {
        if flag {
            observed = 7
            break
        } else {
            observed = 9
            break
        }
    }
    observed
}

fn conditionalRangeBreak(flag Bool) Int {
    var observed = 0
    for index in 0..2 {
        if flag {
            observed = 11
            break
        } else {
            observed = 13
            break
        }
    }
    observed
}

fn deferredLoopExits() Int {
    var index = 0
    var total = 0
    while index < 2 {
        index = index + 1
        defer {
            total = total + 1
        }
        continue
    }
    while true {
        defer {
            total = total + 10
        }
        break
    }
    for item in 0..2 {
        defer {
            total = total + 100
        }
        Unit
    }
    total
}

fn mutationOnContinueBackedge() Int {
    var total = 0
    for index in 0..3 {
        if index < 2 {
            total = total + 1
            continue
        } else {}
    }
    total
}

fn divergentConditionRunsCleanupOnce() Int {
    var cleanupRuns = 0
    defer {
        if cleanupRuns != 0 {
            discard 1 / 0
        } else {}
        cleanupRuns = 1
    }
    while {
        return 7
    } {}
}

pub fn main() {
    let first = whileTotal()
    let second = rangeTotal()
    let whileTrue = conditionalWhileBreak(true)
    let whileFalse = conditionalWhileBreak(false)
    let rangeTrue = conditionalRangeBreak(true)
    let rangeFalse = conditionalRangeBreak(false)
    let deferred = deferredLoopExits()
    let continued = mutationOnContinueBackedge()
    let divergent = divergentConditionRunsCleanupOnce()
    assert first == 43
    assert second == 8
    assert whileTrue == 7
    assert whileFalse == 9
    assert rangeTrue == 11
    assert rangeFalse == 13
    assert deferred == 212
    assert continued == 2
    assert divergent == 7
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
    let lcir = emit_and_run_lcir(&artifact, "source-loop-control");
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-loop-control");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(checked_mir.stdout, lcir.output.stdout);
    assert!(lcir.ir.contains("add nsw i64"), "{}", lcir.ir);
}

#[test]
fn await_operand_control_propagates_to_the_enclosing_function_or_loop() {
    let source = r"async fn work() Int { 7 }

async fn earlyReturn() Int {
    discard (if true { return 5 } else { work() }).await
    6
}

async fn control() Int {
    while true {
        discard (if true { break } else { work() }).await
    }
    var index = 0
    while index < 1 {
        index = index + 1
        discard (if true { continue } else { work() }).await
    }
    9
}

pub async fn main() {
    let returned = earlyReturn().await
    assert returned == 5
    let observed = control().await
    assert observed == 9
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
    let native = emit_and_run_lcir(&artifact, "await-operand-loop-control");
    let checked_mir =
        emit_and_run_checked_mir(&program, "main", "checked-mir-await-operand-loop-control");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(native.output.stdout, b"Unit\n");
    assert_eq!(checked_mir.stdout, native.output.stdout);
}

#[test]
fn loop_witness_flows_keep_conditional_break_and_continue_dispatch_reachable() {
    let source = r"dyn concept Metric {
    method read(self) Int
}

record Counter { value Int }
record Offset { value Int }

impl Metric for Counter {
    method read(self) Int { self.value }
}

impl Metric for Offset {
    method read(self) Int { self.value + 100 }
}

fn counter(value Int) dyn Metric { Counter { value = value } }
fn offset(value Int) dyn Metric { Offset { value = value } }

fn afterBreak(flag Bool) Int {
    var metric = counter(1)
    while true {
        if flag {
            metric = offset(2)
            break
        } else {
            break
        }
    }
    metric.read()
}

fn afterContinue() Int {
    var index = 0
    var metric = counter(3)
    while index < 2 {
        index = index + 1
        if index == 1 {
            metric = offset(4)
            continue
        } else {
            break
        }
    }
    metric.read()
}

pub fn main() {
    let first = afterBreak(false)
    let second = afterBreak(true)
    let third = afterContinue()
    assert first == 1
    assert second == 102
    assert third == 104
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
            .representations()
            .dynamics()
            .iter()
            .any(|dynamic| dynamic.candidates().len() == 2),
        "both loop-carried witnesses must remain reachable"
    );
    let lcir = emit_and_run_lcir(&artifact, "loop-witness-flow");
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-loop-witness-flow");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
}

#[test]
fn mutable_dynamic_reborrow_writeback_is_carried_across_loops() {
    let source = r"dyn concept Stepper {
    method step(mut self) Int
}

record Counter { value Int }

impl Stepper for Counter {
    method step(mut self) Int {
        self.value = self.value + 1
        self.value
    }
}

fn stepMany(value Stepper) Int {
    var index = 0
    while index < 3 {
        index = index + 1
        discard value.step()
    }
    value.step()
}

pub fn main() {
    var counter = Counter { value = 0 }
    let observed = stepMany(counter)
    assert observed == 4
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
    assert!(dump.matches("inout=[0]").count() >= 2, "{dump}");
    let native = emit_and_run_lcir(&artifact, "mutable-dyn-loop-writeback");
    let checked_mir =
        emit_and_run_checked_mir(&program, "main", "checked-mir-mutable-dyn-loop-writeback");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(native.output.stdout, b"Unit\n");
    assert_eq!(checked_mir.stdout, native.output.stdout);
}

#[test]
fn recursive_and_iterative_source_computation_agree_across_backends() {
    let source = r"fn recursive(value Int) Int {
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

fn requireEqual(actual Int, expected Int) {
    if actual == expected {
        Unit
    } else {
        discard 1 / 0
        Unit
    }
}

pub fn main() {
    requireEqual(recursive(10), 55)
    requireEqual(iterative(10), 55)
    requireEqual(highBit(), 9223372036854775806)
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-fibonacci");

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
    assert_fallible_surface(&lcir.ir);
}

#[test]
fn source_short_circuit_never_executes_its_faulting_rhs() {
    let source = r"fn trap() Bool {
    discard 1 / 0
    true
}

pub fn main() {
    discard false && trap()
    discard true || trap()
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-short-circuit");

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
    assert_fallible_surface(&lcir.ir);
}

#[test]
fn source_integer_faults_match_interpreter_and_checked_mir_diagnostics() {
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
        let source = format!("pub fn main() {{\n    {statement}\n}}\n");
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
        let checked_mir =
            emit_and_run_checked_mir(&program, "main", &format!("checked-mir-{name}"));

        assert!(!lcir.output.status.success(), "{name}: {:?}", lcir.output);
        assert!(!checked_mir.status.success(), "{name}: {checked_mir:?}");
        assert!(
            diagnostic_text(&lcir.output).contains(expected),
            "{name}: {:?}",
            lcir.output
        );
        assert!(
            diagnostic_text(&checked_mir).contains(expected),
            "{name}: {checked_mir:?}"
        );
        assert_fallible_surface(&lcir.ir);
    }
}

#[test]
fn loop_control_keeps_legacy_integer_checks_fail_closed() {
    let source = r"fn divideAfterBreak(flag Bool) Int {
    var denominator = 1
    while flag {
        denominator = 0
        break
        denominator = 1
    }
    1 / denominator
}

pub fn main() {
    discard divideAfterBreak(true)
}
";
    let program = compile_source(source);
    let failure = interpret_run(&program, "main").expect_err("division must fault");
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
    assert!(
        dump_program(artifact.program()).contains("checked_int.divide"),
        "typed LCIR must retain the runtime division check"
    );
    let lcir = emit_and_run_lcir(&artifact, "loop-control-division-fault");
    let checked_mir =
        emit_and_run_checked_mir(&program, "main", "checked-mir-loop-control-division-fault");
    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    assert!(!checked_mir.status.success(), "{checked_mir:?}");
    assert!(
        diagnostic_text(&lcir.output).contains("IntegerDivisionByZero"),
        "{:?}",
        lcir.output
    );
    assert!(
        diagnostic_text(&checked_mir).contains("IntegerDivisionByZero"),
        "{checked_mir:?}"
    );
}

#[test]
fn repeated_while_iterations_keep_legacy_integer_checks_fail_closed() {
    let source = r"fn divideOnSecondIteration() Int {
    var denominator = 2
    var index = 0
    while index < 2 {
        denominator = denominator - 1
        index = index + 1
        discard 1 / denominator
    }
    0
}

pub fn main() {
    discard divideOnSecondIteration()
}
";
    let program = compile_source(source);
    let failure = interpret_run(&program, "main").expect_err("second division must fault");
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
    assert!(
        dump_program(artifact.program()).contains("checked_int.divide"),
        "typed LCIR must retain the repeated runtime division check"
    );
    let lcir = emit_and_run_lcir(&artifact, "while-division-fault");
    let checked_mir =
        emit_and_run_checked_mir(&program, "main", "checked-mir-while-division-fault");
    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    assert!(!checked_mir.status.success(), "{checked_mir:?}");
    assert!(
        diagnostic_text(&lcir.output).contains("IntegerDivisionByZero"),
        "{:?}",
        lcir.output
    );
    assert!(
        diagnostic_text(&checked_mir).contains("IntegerDivisionByZero"),
        "{checked_mir:?}"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate keeps all lexical cleanup exit shapes and exact fault metadata together"
)]
fn typed_lexical_cleanup_matches_interpreter_and_checked_mir_on_every_exit_shape() {
    let source = r"fn requireEqual(actual Int, expected Int) {
    assert actual == expected
}

pub fn normalMain() {
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
}

pub fn earlyReturnMain() {
    defer {
        assert false
        Unit
    }
    return
}

pub fn bodyFaultMain() {
    defer {
        assert false
        Unit
    }
    let primary = 1 / 0
    discard primary
}

pub fn cleanupFaultMain() {
    defer {
        let secondary = 1 / 0
        discard secondary
        Unit
    }
    defer {
        assert false
        Unit
    }
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
        let checked_mir = emit_and_run_checked_mir_machine_fault(
            &program,
            entry,
            &format!("checked-mir-cleanup-{entry}"),
        );
        assert!(lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert!(checked_mir.status.success(), "{entry}: {checked_mir:?}");
        assert_eq!(lcir.output.stdout, checked_mir.stdout, "{entry}");
        assert_eq!(lcir.output.stderr, checked_mir.stderr, "{entry}");
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
        let checked_mir = emit_and_run_checked_mir_machine_fault(
            &program,
            entry,
            &format!("checked-mir-cleanup-{entry}"),
        );
        assert!(!lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert!(!checked_mir.status.success(), "{entry}: {checked_mir:?}");
        let lcir_fault = machine_fault(&lcir.output);
        let checked_mir_fault = machine_fault(&checked_mir);
        let code = |fault: &serde_json::Value| {
            fault["fault"]["code"]
                .as_str()
                .or_else(|| fault["code"].as_str())
                .map(str::to_owned)
        };
        assert_eq!(code(&lcir_fault).as_deref(), Some(expected), "LCIR {entry}");
        assert_eq!(
            code(&checked_mir_fault).as_deref(),
            Some(expected),
            "checked-MIR {entry}"
        );
        if expected == "AssertionFault" {
            assert_eq!(
                checked_mir_fault, interpreted,
                "checked-MIR metadata {entry}"
            );
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
    let source = r"import std.resource.Dispose
import std.resource.MustScope

record Resource {
    value Int
}

impl Dispose for Resource {
    method dispose(mut self) {
        let acquired = self.value
        assert acquired > 0
        self.value = 0
    }
}

impl MustScope for Resource {}

pub fn successMain() {
    scoped resource = Resource { value = 3 }
}

pub fn disposalFaultMain() {
    scoped resource = Resource { value = 0 }
}

pub fn initializerFaultMain() {
    scoped resource = Resource { value = 1 / 0 }
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
        let checked_mir = emit_and_run_checked_mir_machine_fault(
            &program,
            entry,
            &format!("checked-mir-scoped-{entry}"),
        );
        assert_eq!(
            lcir.output.status.success(),
            succeeds,
            "{entry}: {:?}",
            lcir.output
        );
        assert_eq!(
            checked_mir.status.success(),
            succeeds,
            "{entry}: {checked_mir:?}"
        );
        assert_eq!(lcir.output.stdout, checked_mir.stdout, "{entry}");
        if succeeds {
            assert_eq!(lcir.output.stderr, checked_mir.stderr, "{entry}");
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
                fault_code(&machine_fault(&checked_mir)).as_deref(),
                expected_fault,
                "checked-MIR {entry}"
            );
        }
        assert_fallible_surface(&lcir.ir);
        assert_no_indirect_calls(&lcir.ir);
    }
}

#[test]
fn integer_overflow_json_matches_at_each_direct_operation_span() {
    let source = r"fn negate(value Int) Int { -value }
fn add(left Int, right Int) Int { left + right }
fn subtract(left Int, right Int) Int { left - right }
fn multiply(left Int, right Int) Int { left * right }

pub fn negateMain() {
    discard negate(-9223372036854775808)
}

pub fn addMain() {
    discard add(9223372036854775807, 1)
}

pub fn subtractMain() {
    discard subtract(-9223372036854775808, 1)
}

pub fn multiplyMain() {
    discard multiply(9223372036854775807, 2)
}
";
    let program = compile_source(source);

    for (entry, operation_name) in [
        ("negateMain", "negate"),
        ("addMain", "add"),
        ("subtractMain", "subtract"),
        ("multiplyMain", "multiply"),
    ] {
        let qualified_operation = format!("standalone.{operation_name}");
        let operation = source_function(&program, &qualified_operation);
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
        let checked_mir = emit_and_run_checked_mir_machine_fault(
            &program,
            entry,
            &format!("checked-mir-integer-overflow-{entry}"),
        );
        assert!(!lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert!(!checked_mir.status.success(), "{entry}: {checked_mir:?}");
        assert_eq!(machine_fault(&lcir.output), expected, "LCIR {entry}");
        assert_eq!(machine_fault(&checked_mir), expected, "checked-MIR {entry}");
        assert_fallible_surface(&lcir.ir);
    }
}

#[test]
fn provable_integer_arithmetic_remains_fault_free() {
    let source = r"pub fn main() {
    let value = (20 + 22) * 1 - 0
    assert value == 42
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
    let checked_mir = emit_and_run_checked_mir_machine_fault_with_ir(
        &program,
        "main",
        "checked-mir-provable-integer-arithmetic",
    );

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(
        checked_mir.output.status.success(),
        "{:?}",
        checked_mir.output
    );
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(checked_mir.output.stdout, lcir.output.stdout);
    assert!(!diagnostic_text(&lcir.output).contains(FAULT_JSON_PREFIX));
    assert!(!diagnostic_text(&checked_mir.output).contains(FAULT_JSON_PREFIX));
    assert!(
        !checked_mir.ir.contains("with.overflow"),
        "provable checked-MIR arithmetic retained a runtime overflow check:\n{}",
        checked_mir.ir
    );
}

#[test]
fn contract_int_negation_overflow_matches_interpreter_lcir_and_checked_mir() {
    let source = r"fn guarded(value Int)
    requires -value >= 0
{
}

fn returnMinimum() Int
    ensures -result >= 0
{
    -9223372036854775808
}

pub fn requiresMain() {
    guarded(-9223372036854775808)
}

pub fn ensuresMain() {
    discard returnMinimum()
}

pub fn assertMain() {
    let minimum = -9223372036854775808
    assert -minimum >= 0
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

        let checked_mir = emit_and_run_checked_mir_machine_fault(
            &program,
            entry,
            &format!("checked-mir-contract-int-negation-{entry}"),
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
        assert!(!checked_mir.status.success(), "{entry}: {checked_mir:?}");
        assert_eq!(machine_fault(&checked_mir), expected, "checked-MIR {entry}");
    }
}

#[test]
fn checked_contract_binary_overflow_and_short_circuit_match_all_backends() {
    let source = r"fn overflow(value Int)
    requires value + 1 > 0
{
}

fn shortCircuit(value Int)
    requires true || value + 1 > 0
{
}

pub fn overflowMain() {
    overflow(9223372036854775807)
}

pub fn shortCircuitMain() {
    shortCircuit(9223372036854775807)
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
    let overflow_checked_mir = emit_and_run_checked_mir_machine_fault(
        &program,
        "overflowMain",
        "checked-mir-contract-binary-overflow",
    );
    assert_eq!(machine_fault(&overflow_lcir.output), expected);
    assert_eq!(machine_fault(&overflow_checked_mir), expected);

    assert_eq!(interpret_run(&program, "shortCircuitMain"), Ok(Value::Unit));
    let safe_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "shortCircuitMain".into(),
        },
    );
    let safe_lcir = emit_and_run_lcir_machine_fault(&safe_artifact, "lcir-contract-short-circuit");
    let safe_checked_mir = emit_and_run_checked_mir_machine_fault(
        &program,
        "shortCircuitMain",
        "checked-mir-contract-short-circuit",
    );
    assert!(safe_lcir.output.status.success(), "{:?}", safe_lcir.output);
    assert!(safe_checked_mir.status.success(), "{safe_checked_mir:?}");
    assert_eq!(safe_lcir.output.stdout, safe_checked_mir.stdout);
}

#[test]
fn contract_precondition_blame_matches_each_closed_world_call_and_checked_root() {
    let source = r"fn positive(value Int)
    requires value > 0
{
}

fn allArgumentsBeforeRequires(first Int, later Int)
    requires first > 0
{
    discard later
}

pub fn callerMain() {
    positive(0)
}

pub fn rootMain()
    requires false
{
}

pub fn argumentFaultMain() {
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
        let checked_mir = emit_and_run_checked_mir_machine_fault(
            &program,
            entry,
            &format!("checked-mir-contract-{entry}"),
        );
        assert!(!lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert!(!checked_mir.status.success(), "{entry}: {checked_mir:?}");
        assert_eq!(machine_fault(&lcir.output), interpreted, "LCIR {entry}");
        assert_eq!(
            machine_fault(&checked_mir),
            interpreted,
            "checked-MIR {entry}"
        );
    }

    let caller = source_function(&program, "callerMain");
    let call = caller.body.tail.as_deref().expect("caller tail call");
    assert!(matches!(call.kind, ExprKind::Call { .. }), "{call:?}");
    let call_span = call.span;
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
    let argument_checked_mir = emit_and_run_checked_mir_machine_fault(
        &program,
        "argumentFaultMain",
        "checked-mir-contract-argument-order",
    );
    assert_eq!(
        machine_fault(&argument_lcir.output)["code"],
        argument_failure["fault"]["code"]
    );
    assert_eq!(
        machine_fault(&argument_checked_mir)["fault"]["code"],
        argument_failure["fault"]["code"]
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate keeps both coroutine call-site blame and the async-root boundary"
)]
fn typed_async_precondition_blame_preserves_each_call_site_and_root_span() {
    let source = r"async fn positive(value Int)
    requires value > 0
{
}
";
    let test_source = r"test async fn a_first_call() {
    positive(0).await
}

test async fn b_second_call() {
    positive(0).await
}

test async fn c_checked_root()
    requires false
{
}
";
    let program = compile_sources(source, test_source);
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(
        interpreted
            .iter()
            .map(|result| result.name.rsplit('.').next().expect("test suffix"))
            .collect::<Vec<_>>(),
        ["a_first_call", "b_second_call", "c_checked_root"]
    );
    let expected = interpreted
        .into_iter()
        .map(|result| {
            assert_eq!(result.status, TestStatus::Failed, "{result:#?}");
            serde_json::to_value(result.failure.expect("precondition must reject"))
                .expect("serialize interpreter precondition fault")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        expected[0]["fault"]["contractSpan"],
        expected[1]["fault"]["contractSpan"]
    );
    assert_ne!(
        expected[0]["fault"]["blameSpan"],
        expected[1]["fault"]["blameSpan"]
    );
    let root = source_function(&program, "c_checked_root");
    assert_eq!(
        expected[2]["fault"]["blameSpan"],
        serde_json::to_value(root.span).expect("serialize async root span")
    );

    let prepared =
        prepare_native_object(&program, EmitOptions::tests(), NativeRoutePolicy::LcirOnly)
            .expect("force the typed LCIR test route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let positive_instances = artifact
        .functions()
        .iter()
        .filter(|function| function.name().ends_with("positive"))
        .collect::<Vec<_>>();
    assert_eq!(
        positive_instances.len(),
        1,
        "the two closed-world calls must share one checked coroutine instance"
    );
    let positive = positive_instances[0];
    assert!(
        positive
            .coroutine()
            .expect("positive coroutine plan")
            .carries_caller_span()
    );
    let call_spans = ["a_first_call", "b_second_call"].map(|suffix| {
        let caller = artifact
            .functions()
            .iter()
            .find(|function| function.name().ends_with(suffix))
            .unwrap_or_else(|| panic!("LCIR test function ending in `{suffix}`"));
        caller
            .instructions()
            .iter()
            .find_map(|instruction| match instruction.kind() {
                InstructionKind::TaskCreate { coroutine, .. } if coroutine == &positive.id() => {
                    Some(instruction.origin().span)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("typed child construction in `{suffix}`"))
    });
    assert_ne!(call_spans[0], call_spans[1]);
    assert_eq!(
        expected[0]["fault"]["blameSpan"],
        serde_json::json!(call_spans[0])
    );
    assert_eq!(
        expected[1]["fault"]["blameSpan"],
        serde_json::json!(call_spans[1])
    );

    let lcir = emit_and_run_lcir_with_options_and_fault_format(
        &artifact,
        "lcir-async-contract-blame",
        NativeObjectOptions::default(),
        true,
    );
    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    assert_eq!(machine_faults(&lcir.output), expected, "typed LCIR faults");

    let checked_mir_directory =
        tempfile::tempdir().expect("create checked-MIR contract test output");
    let checked_mir_executable = checked_mir_directory
        .path()
        .join("checked-mir-async-contract-blame");
    emit_native(&program, &checked_mir_executable, &EmitOptions::tests())
        .expect("emit checked-MIR async contract comparison");
    let checked_mir = Command::new(checked_mir_executable)
        .env(FAULT_FORMAT_ENV, FAULT_FORMAT_JSON)
        .output()
        .expect("run checked-MIR async contract comparison");
    assert!(!checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(
        machine_faults(&checked_mir),
        expected,
        "checked-MIR LLVM faults"
    );

    let descriptor_name = format!("@loom.lcir.coroutine.descriptor.{} =", positive.id().raw());
    assert_eq!(
        lcir.ir.matches(&descriptor_name).count(),
        1,
        "one coroutine instance must own one descriptor:\n{}",
        lcir.ir
    );
    let constructor_name = format!("@loom.lcir.fn.{}(", positive.id().raw());
    assert_eq!(
        lcir.ir
            .lines()
            .filter(|line| {
                line.starts_with("define internal ptr ") && line.contains(&constructor_name)
            })
            .count(),
        1,
        "the shared coroutine constructor must be emitted once:\n{}",
        lcir.ir
    );
    let constructor_calls = lcir
        .ir
        .lines()
        .filter(|line| line.contains("call ptr") && line.contains(&constructor_name))
        .collect::<Vec<_>>();
    assert_eq!(constructor_calls.len(), 2, "{constructor_calls:#?}");
    for span in call_spans {
        let encoded = format!(
            "i64 {}, i64 {}, i64 {}",
            span.file.0, span.range.start, span.range.end
        );
        assert!(
            constructor_calls.iter().any(|call| call.contains(&encoded)),
            "constructor calls omitted `{encoded}`: {constructor_calls:#?}"
        );
    }
    assert!(
        lcir.ir
            .contains("call i32 @loom_context_raise_fault_with_span_v1"),
        "typed coroutine preconditions must use the dynamic-span fault ABI:\n{}",
        lcir.ir
    );
}

#[test]
fn mutable_receiver_old_current_and_cleanup_order_match_all_backends() {
    let source = r"record Boxed { value Int }

fn requireEqual(actual Int, expected Int) {
    assert actual == expected
}

impl Boxed {
    method replaceAfterCleanup(mut self, target Int)
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
    method increase(mut self, amount Int)
        requires amount >= 0
        ensures old(self.value) == 2
        ensures self.value == 5
    {
        self.value = self.value + amount
    }
}

pub fn main() {
    var boxed = Boxed { value = 1 }
    boxed.replaceAfterCleanup(7)
    requireEqual(boxed.value, 7)

    var counter = Counter { value = 2 }
    counter.increase(3)
    requireEqual(counter.value, 5)
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
    let checked_mir = emit_and_run_checked_mir_machine_fault(
        &program,
        "main",
        "checked-mir-contract-mutable-receiver",
    );
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
    assert_eq!(lcir.output.stderr, checked_mir.stderr);
    assert!(!lcir.ir.contains("loom.Value"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("loom_executor_"), "{}", lcir.ir);
}

#[test]
fn cleanup_fault_precedes_the_postcondition_and_matches_all_backends() {
    let source = r"fn failDuringCleanup()
    ensures false
{
    defer {
        discard 1 / 0
    }
}

pub fn main() {
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
    let checked_mir = emit_and_run_checked_mir_machine_fault(
        &program,
        "main",
        "checked-mir-contract-cleanup-fault",
    );
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
    let checked_mir_fault = machine_fault(&checked_mir);
    assert_eq!(
        checked_mir_fault["fault"]["code"],
        interpreted["fault"]["code"]
    );
    assert_eq!(
        checked_mir_fault["fault"]["message"],
        interpreted["fault"]["message"]
    );
    assert!(!lcir.ir.contains("loom.Value"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("loom_executor_"), "{}", lcir.ir);
}

#[test]
fn nested_contract_matches_and_static_concept_calls_match_all_backends() {
    let source = r"concept Source {
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

pub fn main() {
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
    let checked_mir =
        emit_and_run_checked_mir(&program, "main", "checked-mir-contract-static-match");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
    assert!(!lcir.ir.contains("loom.Value"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("loom_executor_"), "{}", lcir.ir);
}

#[test]
fn managed_text_product_remains_typed_and_live_through_a_contract_check() {
    let source = r#"record Label { value Text }

fn accept(label Label, pressure Text)
    requires label.value == "Keep"
{
    discard pressure.length()
}

pub fn main() {
    let label = Label { value = "K".concat("eep") }
    accept(label, "x".concat("y"))
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
    let checked_mir =
        emit_and_run_checked_mir(&program, "main", "checked-mir-contract-managed-product");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
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
    let source = r"test fn zeta() {}
test fn alpha() {}
test fn middle() {}
";
    let program = compile_sources("", source);
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 3);
    assert!(
        interpreted
            .iter()
            .all(|result| result.status == TestStatus::Passed),
        "{interpreted:#?}"
    );
    assert_eq!(
        interpreted
            .iter()
            .map(|result| result.name.as_str())
            .collect::<Vec<_>>(),
        ["standalone.zeta", "standalone.alpha", "standalone.middle",]
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
        ["standalone.zeta", "standalone.alpha", "standalone.middle",]
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
            "passed standalone.zeta",
            "passed standalone.alpha",
            "passed standalone.middle",
        ]
    );
    assert_pure_surface(&native.ir);
}

#[test]
fn source_pod_records_use_direct_ssa_products_and_functional_receiver_writeback() {
    let source = r"record Counter {
    total Int
    calls Int
}

record Holder {
    counter Counter
    enabled Bool
}

impl Holder {
    method setTotal(mut self, value Int) {
        self.counter.total = value
    }
}

impl Counter {
    method add(mut self, value Int) {
        self.total = self.total + value
        self.calls = self.calls + 1
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

fn requireEqual(actual Int, expected Int) {
    if actual == expected {
        Unit
    } else {
        discard 1 / 0
        Unit
    }
}

pub fn main() {
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-records");

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
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
    let program = compile_sources(PROJECTED_PLACE_SOURCE, PROJECTED_PLACE_TEST_SOURCE);
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-projected-places");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
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
    let source = r"record Counter { value Int }
record Pair { left Counter, right Counter }
record Holder { guard Int, pair Pair }

impl Counter {
    method add(mut self, amount Int) {
        self.value = self.value + amount
    }
}

impl Pair {
    method bumpLeft(mut self) {
        self.left.add(5)
    }
}

pub fn main() {
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
    let checked_mir =
        emit_and_run_checked_mir(&program, "main", "checked-mir-nested-receiver-aliases");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
}

#[test]
fn projected_place_products_emit_exact_i686_and_msvc_objects() {
    let program = compile_sources(PROJECTED_PLACE_SOURCE, PROJECTED_PLACE_TEST_SOURCE);
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
    let source = r"record Packet { pair (Int, Bool) }

fn rearrange(input (Packet, Float)) (Bool, Packet) {
    let packet, ignored = input
    discard ignored
    let number, enabled = packet.pair
    (enabled, Packet { pair = (number, enabled) })
}

fn requireEqual(actual Int, expected Int) {
    if actual == expected { Unit } else {
        discard 1 / 0
        Unit
    }
}

pub fn main() {
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
    let (program, debug_sources) = compile_source_with_debug_sources(source);
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
            debug_sources,
            ..NativeObjectOptions::default()
        },
    );
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-tuples");

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
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
    let source = r"type Money = Float where self >= 0.0

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

pub fn main() {
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
    let (program, debug_sources) = compile_source_with_debug_sources(source);
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-refined");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
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
        NativeObjectOptions::default().with_debug_sources(debug_sources),
    );
    assert!(debug.output.status.success(), "{:?}", debug.output);
    assert_eq!(debug.output.stdout, checked_mir.stdout);
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
    let source = r"type Money = Float where self >= 0.0

fn checked(raw Float) Result[Money, ConstraintError] {
    Money(raw)
}

pub fn main() {
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-runtime-refinement");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
    assert_stateless_direct_lcir_surface(&lcir.ir);
    assert!(!lcir.ir.contains("loom_executor_"), "{}", lcir.ir);
}

#[test]
fn generic_runtime_record_invariants_match_checked_mir_without_universal_values() {
    let source = r#"record Guarded[T] {
    value Option[T]
    marker Int

    invariant match self.value {
        Some(value) => self.marker + 1 > 0
        None => false
    }
}

fn checked[T](value Option[T], marker Int) Result[Guarded[T], ConstraintError] {
    Guarded { value = value, marker = marker }
}

pub fn main() {
    match checked[Text](Some("typed"), 1) {
        Ok(value) => { assert value.marker == 1 }
        Err(_) => { assert false }
    }
    match checked[Text](None, 1) {
        Ok(_) => { assert false }
        Err(_) => { assert true }
    }
    match checked[Text](Some("typed"), -1) {
        Ok(_) => { assert false }
        Err(_) => { assert true }
    }
}
"#;
    let program = compile_source(source);
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare generic runtime invariant route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("[Text]"), "{dump}");
    assert!(dump.contains("invariant_record.proven"), "{dump}");
    assert!(dump.contains("InvariantViolation"), "{dump}");

    let lcir = emit_and_run_lcir(&artifact, "generic-runtime-invariant");
    let checked_mir =
        emit_and_run_checked_mir(&program, "main", "checked-mir-generic-runtime-invariant");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
    assert_typed_lcir_surface(&lcir.ir);
    assert_no_indirect_calls(&lcir.ir);
}

#[test]
fn release_tuple_ir_needs_no_storage_runtime_or_executor_surface() {
    let source = r"record Packet { pair (Int, Bool) }

fn roundTrip(input (Packet, Float)) (Bool, Packet) {
    let packet, ignored = input
    discard ignored
    let number, enabled = packet.pair
    (enabled, Packet { pair = (number, enabled) })
}

pub fn main() {
    let enabled, packet = roundTrip((Packet { pair = (40, true) }, 1.5))
    discard enabled
    let number, copied = packet.pair
    discard number
    discard copied
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
    let source = r"enum Single { Wrapped(Int) }

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

fn requireEqual(actual Int, expected Int) {
    if actual == expected { Unit } else {
        discard 1 / 0
        Unit
    }
}

pub fn main() {
    requireEqual(unwrap(Single.Wrapped(41)), 41)
    requireEqual(flag(Flag.Off), 0)
    requireEqual(flag(Flag.On), 1)
    requireEqual(odd(Odd.Empty), 0)
    requireEqual(odd(Odd.Wide(0)), 700)
    requireEqual(odd(Odd.Wide(73)), 73)
    requireEqual(odd(Odd.Bytes(false, false, false, false, false, false, false, false, true)), 9)
    requireEqual(container(Container.Boxed(Envelope { value = Odd.Wide(81) })), 81)
    requireEqual(container(Container.Paired((12, true))), 12)
}
";
    let (program, debug_sources) = compile_source_with_debug_sources(source);
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
            debug_sources,
            ..NativeObjectOptions::default()
        },
    );
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-sums");

    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
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
    assert_stateless_direct_lcir_surface(&lcir.ir);
}

#[test]
fn release_sum_ir_eliminates_carrier_scratch_and_runtime_surfaces() {
    let source = r"enum Odd {
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

pub fn main() {
    discard score(Odd.Empty)
    discard score(Odd.Wide(73))
    discard score(Odd.Bytes(false, false, false, false, false, false, false, false, true))
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
    let program = compile_sources(LIVE_SUM_CARRIER_SOURCE, LIVE_SUM_CARRIER_TEST_SOURCE);
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
fn release_keeps_oppositely_interleaved_managed_sum_carriers_in_register_ssa() {
    let program = compile_sources(
        INTERLEAVED_MANAGED_SUM_RELEASE_SOURCE,
        INTERLEAVED_MANAGED_SUM_RELEASE_TEST_SOURCE,
    );
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
        "release-interleaved-managed-sum",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );

    assert!(release.output.status.success(), "{:?}", release.output);
    assert!(
        release.ir.contains(" phi { i8, {") || release.ir.contains("insertvalue { i8, {"),
        "managed carrier did not remain an aggregate SSA value:\n{}",
        release.ir
    );
    for forbidden in ["alloca { i8, {", "llvm.memcpy", "loom.Value"] {
        assert!(
            !release.ir.contains(forbidden),
            "unexpected carrier scratch surface `{forbidden}` in managed release IR:\n{}",
            release.ir
        );
    }
    assert_no_indirect_calls(&release.ir);
}

#[test]
fn closed_sum_carriers_emit_as_native_msvc_objects_without_fallback() {
    let program = compile_sources(LIVE_SUM_CARRIER_SOURCE, LIVE_SUM_CARRIER_TEST_SOURCE);
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
fn result_unit_test_outcomes_drive_native_and_checked_mir_harnesses() {
    let source = "enum Problem { Failed(Int) }\n";
    let test_source = r"test fn succeeds() Result[Unit, Problem] { Ok(Unit) }

test fn fails() Result[Unit, Problem] { Err(Problem.Failed(7)) }
";
    let program = compile_sources(source, test_source);
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 2);
    assert_eq!(
        interpreted
            .iter()
            .find(|result| result.name == "standalone.succeeds")
            .expect("success test")
            .status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    assert_eq!(
        interpreted
            .iter()
            .find(|result| result.name == "standalone.fails")
            .expect("failure test")
            .status,
        TestStatus::Failed,
        "{interpreted:#?}"
    );

    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let lcir = emit_and_run_lcir(&artifact, "result-tests");
    let checked_mir = emit_and_run_checked_mir_tests(&program, "checked-mir-result-tests");
    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    assert!(!checked_mir.status.success(), "{checked_mir:?}");
    let expected = b"passed standalone.succeeds\nfailed standalone.fails\n";
    assert_eq!(lcir.output.stdout, expected);
    assert_eq!(checked_mir.stdout, expected);
    assert!(lcir.ir.contains("test.result.succeeded"), "{}", lcir.ir);
    assert_no_indirect_calls(&lcir.ir);
    assert_pure_surface(&lcir.ir);
}

#[test]
fn fallible_result_test_checks_runtime_status_before_the_sum_outcome() {
    let source = "enum Problem { Failed }\n";
    let test_source = r"test fn passes() Result[Unit, Problem] { Ok(Unit) }

test fn faults() Result[Unit, Problem] {
    discard 1 / 0
    Ok(Unit)
}
";
    let (program, debug_sources) = compile_sources_with_debug_sources(source, test_source);
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 2);
    assert_eq!(
        interpreted
            .iter()
            .find(|result| result.name == "standalone.passes")
            .expect("passing test")
            .status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    assert_eq!(
        interpreted
            .iter()
            .find(|result| result.name == "standalone.faults")
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
            debug_sources,
            ..NativeObjectOptions::default()
        },
    );
    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    let stdout = String::from_utf8(lcir.output.stdout).expect("UTF-8 LCIR test output");
    assert!(stdout.contains("passed standalone.passes"), "{stdout}");
    assert!(stdout.contains("failed standalone.faults"), "{stdout}");
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
    let source = r"record Counter { value Int }
record Pair { left Counter, right Counter }
record Holder { pair Pair, guard Int }

impl Counter {
    method mutateThenFail(mut self) {
        self.value = 9
        discard 1 / 0
    }
}

impl Holder {
    method cascade(mut self) {
        self.pair.left.mutateThenFail()
    }
}

pub fn main() {
    var holder = Holder {
        pair = Pair {
            left = Counter { value = 1 },
            right = Counter { value = 2 },
        },
        guard = 7,
    }
    holder.cascade()
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
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-record-fault");

    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    assert!(!checked_mir.status.success(), "{checked_mir:?}");
    assert!(
        diagnostic_text(&lcir.output).contains("IntegerDivisionByZero"),
        "{:?}",
        lcir.output
    );
    assert!(
        diagnostic_text(&checked_mir).contains("IntegerDivisionByZero"),
        "{checked_mir:?}"
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

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate keeps typed coroutine planning, forced parent-root relocation, run/test harnesses, ABI shape, and cross-target object emission together"
)]
fn typed_async_state_machines_survive_forced_relocation_on_all_targets() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-async/main.loom"),
        include_str!("../../../fixtures/lcir-typed-async/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare automatic typed-async route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let unsupported = lower_typed_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(32).expect("32-bit target layout"),
    )
    .expect("classify typed async for 32-bit target");
    assert!(
        matches!(unsupported, LoweringOutcome::Unsupported(_)),
        "Task handles must fail closed outside the pinned 64-bit runtime ABI: {unsupported:?}"
    );

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("typed async main instance");
    let pressure = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("allocationPressure"))
        .expect("typed allocation-pressure child instance");
    let precreated = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("precreatedChildren"))
        .expect("typed pre-created-child coroutine instance");
    assert!(pressure.coroutine().is_some());
    assert!(pressure.effects().contains(Effects::MAY_COLLECT));
    let plan = main.coroutine().expect("typed async main coroutine plan");
    assert_eq!(
        plan.suspensions()
            .iter()
            .map(loom_codegen_ir::CoroutineSuspension::state)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(plan.suspensions()[0].live().iter().any(|ty| {
        artifact
            .representations()
            .value_type(*ty)
            .and_then(|value| artifact.representations().repr(value.repr()))
            == Some(&loom_codegen_ir::Repr::ManagedPointer)
    }));
    let precreated_plan = precreated
        .coroutine()
        .expect("pre-created children use a checked coroutine plan");
    assert_eq!(
        precreated_plan
            .suspensions()
            .iter()
            .map(loom_codegen_ir::CoroutineSuspension::state)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(precreated_plan.suspensions()[0].live().iter().any(|ty| {
        artifact
            .representations()
            .value_type(*ty)
            .and_then(|value| artifact.representations().repr(value.repr()))
            == Some(&loom_codegen_ir::Repr::TaskHandle)
    }));

    let lcir = emit_and_run_lcir(&artifact, "source-typed-async");
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-typed-async");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
    assert_eq!(lcir.output.stderr, checked_mir.stderr);
    for required in [
        "loom.lcir.coroutine.resume.",
        "loom.lcir.coroutine.descriptor.",
        "loom_typed_task_create_v1",
        "loom_typed_task_set_root_state_v1",
        "loom_typed_task_publish_result_v1",
        "loom_typed_task_take_result_v1",
        "loom_task_prepare_join",
        "loom_task_add_join_child",
        "loom_task_suspend_join",
        "loom_executor_run",
    ] {
        assert!(
            lcir.ir.contains(required),
            "missing `{required}`:\n{}",
            lcir.ir
        );
    }
    assert!(
        lcir.ir.contains("task.await.immediate.ready"),
        "{}",
        lcir.ir
    );
    assert!(
        lcir.ir.matches("task.await.state.pointer").count() >= 6,
        "{}",
        lcir.ir
    );
    assert!(lcir.ir.contains("task.await.live.0.pointer"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("%loom.Value"), "{}", lcir.ir);
    assert!(!lcir.ir.contains("@loom.fn."), "{}", lcir.ir);

    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    let test_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native_tests = emit_and_run_lcir(&test_artifact, "source-typed-async-tests");
    let checked_mir_tests =
        emit_and_run_checked_mir_tests(&program, "checked-mir-typed-async-tests");
    assert!(
        native_tests.output.status.success(),
        "{:?}",
        native_tests.output
    );
    assert!(checked_mir_tests.status.success(), "{checked_mir_tests:?}");
    assert_eq!(native_tests.output.stdout, checked_mir_tests.stdout);
    assert_eq!(native_tests.output.stderr, checked_mir_tests.stderr);

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create typed-async target output");
        let object = directory.path().join(if target.contains("windows") {
            "typed-async.obj"
        } else {
            "typed-async.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit typed async object for {target}: {error}"));
        assert!(object.is_file(), "missing typed async object for {target}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate keeps List/TextMap coroutine signatures, suspension roots, debug types, relocation pressure, and cross-target objects together"
)]
fn managed_collections_are_exact_typed_coroutine_frame_carriers() {
    let (program, debug_sources) = compile_sources_with_debug_sources(
        include_str!("../../../fixtures/lcir-async-managed-collections/main.loom"),
        include_str!("../../../fixtures/lcir-async-managed-collections/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );

    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare automatic managed-collection coroutine route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let request = SourceArtifactRequest::Run {
        entry: "main".into(),
    };
    let unsupported = lower_typed_artifact(
        &program,
        &request,
        TargetLayout::new(32).expect("32-bit target layout"),
    )
    .expect("classify managed-collection coroutine for a 32-bit target");
    let LoweringOutcome::Unsupported(report) = unsupported else {
        panic!(
            "managed coroutine pointers must fail closed outside the pinned 64-bit ABI: {unsupported:?}"
        )
    };
    assert!(
        report.items().iter().any(|item| matches!(
            item.feature(),
            UnsupportedFeature::SignatureType
                | UnsupportedFeature::ExpressionType
                | UnsupportedFeature::Suspension
        )),
        "32-bit managed coroutine report omitted the rejected pointer site: {report:#?}"
    );

    let artifact = lower_source_artifact(&program, &request);
    let carry = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("carry"))
        .expect("managed-collection carrier coroutine");
    let carry_plan = carry.coroutine().expect("checked carrier coroutine plan");
    assert_eq!(carry_plan.suspensions().len(), 2);
    let first_live = carry_plan.suspensions()[0]
        .live()
        .iter()
        .filter_map(|ty| artifact.representations().value_type(*ty))
        .collect::<Vec<_>>();
    assert!(
        first_live
            .iter()
            .any(|value| matches!(value.semantic(), Type::List(_)))
    );
    assert!(
        first_live
            .iter()
            .any(|value| { value.kind() == loom_codegen_ir::ValueTypeKind::ManagedTextMap })
    );
    for value in first_live.iter().filter(|value| {
        matches!(value.semantic(), Type::List(_))
            || value.kind() == loom_codegen_ir::ValueTypeKind::ManagedTextMap
    }) {
        assert_eq!(
            artifact.representations().repr(value.repr()),
            Some(&Repr::ManagedPointer)
        );
    }

    let lcir = emit_and_run_lcir_with_options(
        &artifact,
        "source-async-managed-collections",
        NativeObjectOptions::default().with_debug_sources(debug_sources),
    );
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert_eq!(lcir.output.stdout, b"Unit\n");
    for required in [
        "loom.lcir.coroutine.resume.",
        "loom.lcir.coroutine.descriptor.",
        "loom.lcir.list.descriptor",
        "loom.lcir.text_map.descriptor",
        "loom_gc_typed_repeated_alloc_v1",
        "task.await.live.",
        "managed.root.reload",
        "TextMapObject",
    ] {
        assert!(
            lcir.ir.contains(required),
            "managed coroutine IR omitted `{required}`:\n{}",
            lcir.ir
        );
    }
    let text_map_debug_type = lcir
        .ir
        .lines()
        .find(|line| line.contains("name: \"TextMapObject\""))
        .expect("TextMap debug object type");
    assert!(
        text_map_debug_type.contains("size: 64") && text_map_debug_type.contains("align: 64"),
        "TextMap debug header must match its one-i64 repeated-object prefix: {text_map_debug_type}"
    );
    let carry_offsets = format!(
        "@loom.lcir.coroutine.root_offsets.{} = private unnamed_addr constant [8 x i64] [i64 8, i64 16, i64 32, i64 40, i64 56, i64 64, i64 80, i64 88]",
        carry.id().raw()
    );
    let carry_bitmaps = format!(
        "@loom.lcir.coroutine.live_bitmaps.{} = private unnamed_addr constant [4 x i64] [i64 3, i64 12, i64 48, i64 192]",
        carry.id().raw()
    );
    assert!(
        lcir.ir.contains(&carry_offsets),
        "List/TextMap frame root offsets changed unexpectedly:\n{}",
        lcir.ir
    );
    assert!(
        lcir.ir.contains(&carry_bitmaps),
        "each carry suspension must publish exactly its checked List/TextMap pair:\n{}",
        lcir.ir
    );
    assert!(!lcir.ir.contains("%loom.Value"), "{}", lcir.ir);

    let test_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native_tests = emit_and_run_lcir(&test_artifact, "source-async-managed-collections-tests");
    assert!(
        native_tests.output.status.success(),
        "{:?}",
        native_tests.output
    );
    assert!(
        String::from_utf8_lossy(&native_tests.output.stdout)
            .contains("managedCollectionsCrossAwait"),
        "{:?}",
        native_tests.output
    );

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create managed coroutine target output");
        let object = directory.path().join(if target.contains("windows") {
            "async-managed-collections.obj"
        } else {
            "async-managed-collections.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit managed coroutine object for {target}: {error}"));
        assert!(
            object.is_file(),
            "missing managed coroutine object for {target}"
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one source gate proves direct, transitive, fallible, composite, debug, native, and cross-target hidden-executor ABI behavior"
)]
fn synchronous_task_helpers_borrow_one_checked_executor_context() {
    let (program, debug_sources) = compile_sources_with_debug_sources(
        include_str!("../../../fixtures/lcir-sync-task-helpers/main.loom"),
        include_str!("../../../fixtures/lcir-sync-task-helpers/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let interpreted_tests = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted_tests.len(), 1, "{interpreted_tests:#?}");
    assert_eq!(
        interpreted_tests[0].status,
        TestStatus::Passed,
        "{interpreted_tests:#?}"
    );

    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare automatic synchronous Task-helper route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let function = |suffix: &str| {
        artifact
            .functions()
            .iter()
            .find(|function| function.name().ends_with(suffix))
            .unwrap_or_else(|| panic!("missing synchronous Task helper `{suffix}`"))
    };
    let direct = function("direct");
    let nested = function("nested");
    let timer = function("timer");
    let nested_timer = function("nestedTimer");
    let combined = function("combined");
    for helper in [direct, nested, timer, nested_timer, combined] {
        assert!(helper.coroutine().is_none(), "{}", helper.name());
        assert!(
            helper.effects().contains(Effects::NEEDS_EXECUTOR)
                && helper.effects().contains(Effects::NEEDS_RUNTIME),
            "{} has {}",
            helper.name(),
            helper.effects()
        );
        assert!(
            !helper.effects().contains(Effects::MAY_SUSPEND),
            "a Task-producing helper must not become a coroutine: {}",
            helper.name()
        );
    }
    for helper in [timer, nested_timer] {
        assert!(helper.effects().contains(Effects::MAY_FAULT));
    }
    for helper in [direct, nested, combined] {
        assert!(!helper.effects().contains(Effects::MAY_FAULT));
    }
    assert!(combined.instructions().iter().any(|instruction| matches!(
        instruction.kind(),
        InstructionKind::TaskJoin {
            mode: loom_codegen_ir::AwaitMode::All,
            ..
        }
    )));

    let lcir = emit_and_run_lcir_with_options(
        &artifact,
        "source-sync-task-helpers",
        NativeObjectOptions::default().with_debug_sources(debug_sources),
    );
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert!(lcir.output.stderr.is_empty(), "{:?}", lcir.output);
    assert!(!lcir.ir.contains("%loom.Value"), "{}", lcir.ir);
    let direct_symbol = format!("@loom.lcir.fn.{}", direct.id().raw());
    let nested_symbol = format!("@loom.lcir.fn.{}", nested.id().raw());
    let timer_symbol = format!("@loom.lcir.fn.{}", timer.id().raw());
    let nested_timer_symbol = format!("@loom.lcir.fn.{}", nested_timer.id().raw());
    let combined_symbol = format!("@loom.lcir.fn.{}", combined.id().raw());
    assert!(
        lcir.ir.contains(&format!(
            "define internal ptr {direct_symbol}(i64 %arg0, ptr %__loom_executor)"
        )),
        "{}",
        lcir.ir
    );
    assert!(
        lcir.ir.contains(&format!(
            "define internal ptr {nested_symbol}(i64 %arg0, ptr %__loom_executor)"
        )),
        "{}",
        lcir.ir
    );
    assert!(
        lcir.ir.contains(&format!(
            "define internal ptr {combined_symbol}(i64 %arg0, i64 %arg1, ptr %__loom_executor)"
        )),
        "{}",
        lcir.ir
    );
    assert!(
        lcir.ir.contains(&format!(
            "define internal {{ i32, ptr }} {timer_symbol}(i64 %arg0, ptr %__loom_fault_context, ptr %__loom_executor)"
        )),
        "{}",
        lcir.ir
    );
    assert!(
        lcir.ir.contains(&format!(
            "define internal {{ i32, ptr }} {nested_timer_symbol}(i64 %arg0, ptr %__loom_fault_context, ptr %__loom_executor)"
        )),
        "{}",
        lcir.ir
    );
    let nested_ir = lcir
        .ir
        .split("\ndefine ")
        .find(|body| body.contains(&format!("{nested_symbol}(")))
        .expect("nested helper IR");
    assert!(
        nested_ir.contains(&format!(
            "call ptr {direct_symbol}(i64 %arg0, ptr %__loom_executor)"
        )),
        "{nested_ir}"
    );
    let nested_timer_ir = lcir
        .ir
        .split("\ndefine ")
        .find(|body| body.contains(&format!("{nested_timer_symbol}(")))
        .expect("nested fallible timer helper IR");
    assert!(
        nested_timer_ir.contains(&format!(
            "call {{ i32, ptr }} {timer_symbol}(i64 %arg0, ptr %__loom_fault_context, ptr %__loom_executor)"
        )),
        "{nested_timer_ir}"
    );
    assert!(!nested_ir.contains("loom_executor_create"), "{nested_ir}");
    assert!(!nested_ir.contains("loom_executor_destroy"), "{nested_ir}");
    assert!(
        !nested_timer_ir.contains("loom_executor_create"),
        "{nested_timer_ir}"
    );
    assert!(
        !nested_timer_ir.contains("loom_executor_destroy"),
        "{nested_timer_ir}"
    );
    for metadata in [
        "name: \"__loom_executor\", arg: 2",
        "name: \"__loom_executor\", arg: 3",
        "name: \"__loom_fault_context\", arg: 2",
        "name: \"LoomFallible<Task>\"",
    ] {
        assert!(
            lcir.ir.contains(metadata),
            "missing `{metadata}` in {}",
            lcir.ir
        );
    }

    let test_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native_tests = emit_and_run_lcir(&test_artifact, "source-sync-task-helper-tests");
    assert!(
        native_tests.output.status.success(),
        "{:?}",
        native_tests.output
    );
    assert!(
        String::from_utf8_lossy(&native_tests.output.stdout)
            .contains("synchronousTaskHelpersBorrowTheCurrentExecutor"),
        "{:?}",
        native_tests.output
    );

    let expected_fault = serde_json::to_value(
        interpret_run(&program, "negativeSleepMain")
            .expect_err("negative synchronous Task.sleep helper must fault"),
    )
    .expect("serialize interpreter fault");
    let fault_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "negativeSleepMain".into(),
        },
    );
    let fault = emit_and_run_lcir_machine_fault(&fault_artifact, "sync-task-helper-fault");
    assert!(!fault.output.status.success(), "{:?}", fault.output);
    assert_eq!(machine_fault(&fault.output), expected_fault);

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create sync-helper target output");
        let object = directory.path().join(if target.contains("windows") {
            "sync-task-helpers.obj"
        } else {
            "sync-task-helpers.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit sync Task-helper object for {target}: {error}"));
        assert!(
            object.is_file(),
            "missing sync Task-helper object for {target}"
        );
    }
}

#[test]
fn typed_sleep_uses_checked_lcir_and_the_narrow_timer_runtime_abi() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-sleep/main.loom"),
        include_str!("../../../fixtures/lcir-typed-sleep/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare automatic typed-sleep route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let unsupported = lower_typed_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(32).expect("32-bit target layout"),
    )
    .expect("classify typed sleep for a 32-bit target");
    assert!(
        matches!(unsupported, LoweringOutcome::Unsupported(_)),
        "typed timer Task handles must fail closed outside the pinned 64-bit ABI: {unsupported:?}"
    );

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("product.extract"), "{dump}");
    assert_eq!(dump.matches("task.sleep").count(), 3, "{dump}");
    for suffix in ["main", "waitInt", "waitDuration"] {
        let function = artifact
            .functions()
            .iter()
            .find(|function| function.name().ends_with(suffix))
            .unwrap_or_else(|| panic!("missing typed-sleep function `{suffix}`"));
        assert!(
            function.effects().contains(Effects::MAY_FAULT)
                && function.effects().contains(Effects::NEEDS_EXECUTOR),
            "{suffix}: {dump}"
        );
    }

    let lcir = emit_and_run_lcir_with_options(
        &artifact,
        "source-typed-sleep-release",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );
    let checked_mir = emit_and_run_checked_mir(&program, "main", "checked-mir-typed-sleep");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
    assert_eq!(lcir.output.stderr, checked_mir.stderr);
    for required in [
        "llvm.smul.with.overflow.i64",
        "llvm.uadd.with.overflow.i64",
        "loom_wait_now_ns",
        "loom_typed_timer_task_create_v1",
    ] {
        assert!(
            lcir.ir.contains(required),
            "missing `{required}` from typed timer release IR:\n{}",
            lcir.ir
        );
    }
    for forbidden in ["loom_task_from_wait_source", "%loom.Value", "loom.Value"] {
        assert!(
            !lcir.ir.contains(forbidden),
            "unexpected `{forbidden}` in typed timer release IR:\n{}",
            lcir.ir
        );
    }

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create typed-sleep target output");
        let object = directory.path().join(if target.contains("windows") {
            "typed-sleep.obj"
        } else {
            "typed-sleep.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit typed sleep object for {target}: {error}"));
        assert!(object.is_file(), "missing typed sleep object for {target}");
    }
}

#[test]
fn typed_sleep_source_faults_match_interpreter_and_checked_mir_codegen() {
    let source = r"pub async fn negativeMain() {
    let timer = Task.sleep(-1)
    timer.await
}

pub async fn overflowMain() {
    let timer = Task.sleep(9223372036854775807)
    timer.await
}
";
    let program = compile_source(source);
    for (entry, code, message) in [
        (
            "negativeMain",
            INVALID_SLEEP_DURATION_FAULT_CODE,
            INVALID_SLEEP_DURATION_FAULT_MESSAGE,
        ),
        (
            "overflowMain",
            SLEEP_DURATION_OVERFLOW_FAULT_CODE,
            SLEEP_DURATION_OVERFLOW_FAULT_MESSAGE,
        ),
    ] {
        let expected = serde_json::to_value(
            interpret_run(&program, entry).expect_err("Task.sleep construction must fault"),
        )
        .expect("serialize interpreter sleep fault");
        assert_eq!(expected["fault"]["code"], code, "interpreter {entry}");
        assert_eq!(expected["fault"]["message"], message, "interpreter {entry}");
        let artifact = lower_source_artifact(
            &program,
            &SourceArtifactRequest::Run {
                entry: entry.into(),
            },
        );
        let lcir = emit_and_run_lcir_machine_fault(&artifact, &format!("lcir-sleep-{entry}"));
        let checked_mir = emit_and_run_checked_mir_machine_fault(
            &program,
            entry,
            &format!("checked-mir-sleep-{entry}"),
        );
        assert!(!lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert!(!checked_mir.status.success(), "{entry}: {checked_mir:?}");
        assert_eq!(machine_fault(&lcir.output), expected, "LCIR {entry}");
        let checked_mir_fault = machine_fault(&checked_mir);
        // The checked-MIR emitter attributes this constructor fault to the
        // whole async function. Keep that coarse-span limitation out of
        // the checked-LCIR slice while pinning every observable fault field.
        assert_eq!(
            checked_mir_fault["channel"], expected["channel"],
            "checked-MIR {entry}"
        );
        assert_eq!(
            checked_mir_fault["fault"]["code"], expected["fault"]["code"],
            "checked-MIR {entry}"
        );
        assert_eq!(
            checked_mir_fault["fault"]["message"], expected["fault"]["message"],
            "checked-MIR {entry}"
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate keeps direct and first-class heterogeneous Task.all lowering, exact moving-GC roots, shape reuse, failure propagation, and cross-target objects together"
)]
fn typed_static_task_all_uses_exact_direct_and_first_class_codegen() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-task-all/main.loom"),
        include_str!("../../../fixtures/lcir-typed-task-all/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare automatic typed Task.all route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let unsupported = lower_typed_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(32).expect("32-bit target layout"),
    )
    .expect("classify typed Task.all for a 32-bit target");
    assert!(
        matches!(unsupported, LoweringOutcome::Unsupported(_)),
        "typed Task handles must fail closed outside the pinned 64-bit ABI: {unsupported:?}"
    );

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("await_tasks"), "{dump}");
    assert!(dump.contains("task.join.all"), "{dump}");
    let exercise = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("exerciseJoins"))
        .expect("typed Task.all exercise coroutine");
    let plan = exercise
        .coroutine()
        .expect("typed Task.all exercise coroutine plan");
    assert!(
        plan.suspensions()
            .iter()
            .any(|suspension| suspension.awaited().len() >= 2),
        "direct heterogeneous await did not retain its exact child-result row: {dump}"
    );
    assert!(
        exercise.effects().contains(Effects::NEEDS_EXECUTOR)
            && !exercise.effects().contains(Effects::MAY_COLLECT),
        "typed Task creation must require its executor without inheriting child collection: {dump}"
    );

    let lcir = emit_and_run_lcir_with_options(
        &artifact,
        "source-typed-task-all-release",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert!(lcir.output.stderr.is_empty(), "{:?}", lcir.output);
    for required in [
        "loom.lcir.task_join.all.resume.",
        "loom.lcir.task_join.all.0.descriptor",
        "loom_typed_task_publish_adopting_v1",
        "loom_typed_task_abort_unpublished_v1",
        "loom_task_prepare_join",
        "loom_task_add_join_child",
        "loom_task_suspend_join",
        "task.await.child.1.pointer",
        "task.await.result.1.take",
    ] {
        assert!(
            lcir.ir.contains(required),
            "missing `{required}` from typed Task.all release IR:\n{}",
            lcir.ir
        );
    }
    assert_eq!(
        lcir.ir
            .lines()
            .filter(|line| {
                line.starts_with("@loom.lcir.task_join.all.") && line.contains(".descriptor =")
            })
            .count(),
        2,
        "two identical sites must share one descriptor while a distinct shape gets another:\n{}",
        lcir.ir
    );
    assert_eq!(
        lcir.ir
            .lines()
            .filter(|line| {
                line.starts_with("define internal")
                    && line.contains(" i32 @loom.lcir.task_join.all.resume.")
            })
            .count(),
        2,
        "two identical sites must share one callback while a distinct shape gets another:\n{}",
        lcir.ir
    );
    assert!(
        lcir.ir.contains(
            "@loom.lcir.task_join.all.0.root_offsets = private unnamed_addr constant [1 x i64] [i64 32]"
        ),
        "the Text-bearing composite must expose its exact completed-result root offset:\n{}",
        lcir.ir
    );
    assert!(
        lcir.ir.contains(
            "@loom.lcir.task_join.all.0.live_bitmaps = private unnamed_addr constant [3 x i64] [i64 0, i64 0, i64 1]"
        ),
        "only the completed composite state may root its initialized Text result:\n{}",
        lcir.ir
    );
    for forbidden in [
        "%loom.Value",
        "loom.Value",
        "loom_join_create",
        "loom_task_write_join_result",
        "loom_task_join_result",
    ] {
        assert!(
            !lcir.ir.contains(forbidden),
            "unexpected `{forbidden}` in typed Task.all release IR:\n{}",
            lcir.ir
        );
    }

    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    let test_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native_tests = emit_and_run_lcir(&test_artifact, "source-typed-task-all-tests");
    assert!(
        native_tests.output.status.success(),
        "{:?}",
        native_tests.output
    );
    assert!(
        native_tests.output.stderr.is_empty(),
        "{:?}",
        native_tests.output
    );

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create typed Task.all target output");
        let object = directory.path().join(if target.contains("windows") {
            "typed-task-all.obj"
        } else {
            "typed-task-all.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit typed Task.all object for {target}: {error}"));
        assert!(
            object.is_file(),
            "missing typed Task.all object for {target}"
        );
    }
}

#[test]
fn typed_fixed_task_any_selects_one_exact_winner_and_reports_all_failed() {
    let source = r#"async fn slow() Text {
    Task.sleep(20).await
    "slow"
}

async fn fast() Text { "fast" }

async fn failed() Text {
    assert false
    "unreachable"
}

pub async fn main() {
    let winner = Task.any(slow(), fast()).await
    assert winner == "fast"
    let recovered = Task.any(failed(), fast()).await
    assert recovered == "fast"
}

pub async fn allFailed() {
    let failedJoin = Task.any(failed(), failed())
    discard failedJoin.await
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
    assert!(dump.contains("await_tasks any"), "{dump}");
    assert!(!dump.contains("task.join."), "{dump}");
    let lcir = emit_and_run_lcir_with_options(
        &artifact,
        "source-typed-task-any-release",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert!(lcir.output.stderr.is_empty(), "{:?}", lcir.output);
    for required in [
        "loom_task_prepare_join",
        "loom_task_join_step",
        "loom_task_join_winner",
        "task.await.any.winner",
        "task.await.any.result.take",
    ] {
        assert!(
            lcir.ir.contains(required),
            "missing `{required}` from typed Task.any release IR:\n{}",
            lcir.ir
        );
    }
    for forbidden in [
        "%loom.Value",
        "loom.Value",
        "loom_join_create",
        "loom_task_write_join_result",
        "loom_task_join_result",
    ] {
        assert!(
            !lcir.ir.contains(forbidden),
            "unexpected `{forbidden}` in typed Task.any release IR:\n{}",
            lcir.ir
        );
    }

    let expected = serde_json::to_value(
        interpret_run(&program, "allFailed").expect_err("Task.any must fault without a winner"),
    )
    .expect("serialize interpreter Task.any fault");
    assert_eq!(expected["fault"]["code"], TASK_ANY_FAILED_FAULT_CODE);
    assert_eq!(expected["fault"]["message"], TASK_ANY_FAILED_FAULT_MESSAGE);
    let failed_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "allFailed".into(),
        },
    );
    let native = emit_and_run_lcir_machine_fault(&failed_artifact, "source-typed-task-any-failed");
    assert!(!native.output.status.success(), "{:?}", native.output);
    assert_eq!(machine_fault(&native.output), expected);
    for required in [
        "loom.lcir.task_join.any.resume.",
        "task.join.any.fault.winner",
        "task.join.any.no_winner",
        TASK_ANY_FAILED_FAULT_CODE,
    ] {
        assert!(
            native.ir.contains(required),
            "stored Task.any omitted `{required}` from typed IR:\n{}",
            native.ir
        );
    }
    for forbidden in ["%loom.Value", "loom.Value", "loom_join_create"] {
        assert!(
            !native.ir.contains(forbidden),
            "stored Task.any retained `{forbidden}` in typed IR:\n{}",
            native.ir
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one source gate keeps settled/race outcome capture, managed fault roots, winner selection, loser cleanup, tests, and cross-target objects together"
)]
fn typed_fixed_task_outcomes_capture_faults_and_race_nonzero_winners() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-task-outcomes/main.loom"),
        include_str!("../../../fixtures/lcir-typed-task-outcomes/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));

    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare automatic typed Task outcome route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    for required in [
        "await_tasks settled",
        "await_tasks race",
        "task.join.settled",
        "task.join.race",
        "task.outcome_take %",
    ] {
        assert!(
            dump.contains(required),
            "typed Task outcome LCIR omitted `{required}`:\n{dump}"
        );
    }
    assert!(
        dump.matches("task.outcome_take %").count() >= 6,
        "direct settled children and race winners must be captured explicitly while stored joins capture inside their typed callback:\n{dump}"
    );

    let lcir = emit_and_run_lcir_with_options(
        &artifact,
        "source-typed-task-outcomes-release",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
    );
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert!(lcir.output.stderr.is_empty(), "{:?}", lcir.output);
    for required in [
        "loom_typed_task_take_outcome_v1",
        "loom_task_prepare_join",
        "loom_task_add_join_child",
        "loom_task_suspend_join",
        "loom_task_join_step",
        "loom_task_join_winner",
        "loom.lcir.task_join.settled.",
        "loom.lcir.task_join.race.",
        "task.join.settled.collecting_root_state",
        "task.join.race.outcome.",
        "task.await.settled.child.2",
        "task.outcome.fault.code",
        "task.outcome.fault.message",
        "coroutine.cancel.live",
        "loom_gc_typed_root_push_v1",
    ] {
        assert!(
            lcir.ir.contains(required),
            "missing `{required}` from typed Task outcome release IR:\n{}",
            lcir.ir
        );
    }
    assert!(
        lcir.ir.lines().any(|line| {
            line.starts_with("@loom.lcir.task_join.settled.")
                && line.contains(".live_bitmaps =")
                && line.contains("[4 x i64] [i64 0, i64 0, i64 15, i64 15]")
        }),
        "stored Task.settled must root both partial and completed fault outcomes:\n{}",
        lcir.ir
    );
    assert!(
        lcir.ir.lines().any(|line| {
            line.starts_with("@loom.lcir.task_join.race.")
                && line.contains(".live_bitmaps =")
                && line.contains("[3 x i64] [i64 0, i64 0, i64 3]")
        }),
        "stored Task.race must root its completed fault outcome:\n{}",
        lcir.ir
    );
    assert_eq!(
        lcir.ir
            .lines()
            .filter(|line| {
                line.starts_with("@loom.lcir.task_join.race.") && line.contains(".descriptor =")
            })
            .count(),
        1,
        "identical stored Task.race shapes must share one descriptor:\n{}",
        lcir.ir
    );
    assert_eq!(
        lcir.ir
            .lines()
            .filter(|line| {
                line.starts_with("define internal")
                    && line.contains(" i32 @loom.lcir.task_join.race.resume.")
            })
            .count(),
        1,
        "identical stored Task.race shapes must share one callback:\n{}",
        lcir.ir
    );
    for forbidden in [
        "%loom.Value",
        "loom.Value",
        "ValueNode",
        "loom_join_create",
        "loom_join_add_task",
        "loom_task_write_join_result",
        "loom_task_join_result",
    ] {
        assert!(
            !lcir.ir.contains(forbidden),
            "unexpected `{forbidden}` in typed Task outcome release IR:\n{}",
            lcir.ir
        );
    }

    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    let test_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native_tests = emit_and_run_lcir(&test_artifact, "source-typed-task-outcomes-tests");
    assert!(
        native_tests.output.status.success(),
        "{:?}",
        native_tests.output
    );
    assert!(
        native_tests.output.stderr.is_empty(),
        "{:?}",
        native_tests.output
    );

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create typed Task outcome target output");
        let object = directory.path().join(if target.contains("windows") {
            "typed-task-outcomes.obj"
        } else {
            "typed-task-outcomes.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit typed Task outcome object for {target}: {error}"));
        assert!(
            object.is_file(),
            "missing typed Task outcome object for {target}"
        );
    }
}

#[test]
fn task_bearing_sums_emit_with_only_managed_payload_roots() {
    let program = compile_source(
        r#"record Payload { label Text, values List[Int] }

enum Work {
    Pending(Task[Int], Payload)
    Idle
}

async fn child() Int { 7 }

pub async fn main() {
    let work = Work.Pending(child(), Payload { label = "queued", values = [1, 2] })
    discard Task.sleep(0).await
    match work {
        Pending(task, payload) => {
            assert payload.label == "queued"
            discard payload.values.length()
            let value = task.await
            assert value == 7
        }
        Idle => Unit
    }
}
"#,
    );
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main instance");
    let directory = tempfile::tempdir().expect("create affine sum output");
    let object = directory.path().join("task-bearing-sum.o");
    let ir_path = directory.path().join("task-bearing-sum.ll");
    emit_lcir_native_object(
        &artifact,
        &object,
        &NativeObjectOptions {
            emit_ir: Some(ir_path.clone()),
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit Task-bearing sum object");
    assert!(object.is_file(), "missing Task-bearing sum object");
    let ir = std::fs::read_to_string(ir_path).expect("read Task-bearing sum IR");
    assert!(ir.contains("sum.switch"), "{ir}");
    let offsets_prefix = format!("@loom.lcir.coroutine.root_offsets.{} =", main.id().raw());
    let offsets = ir
        .lines()
        .find(|line| line.starts_with(&offsets_prefix))
        .unwrap_or_else(|| panic!("missing `{offsets_prefix}`:\n{ir}"));
    assert!(
        offsets.contains("[2 x i64] [i64 32, i64 40]"),
        "the live Work(Task, Payload(Text, List)) frame must trace only Text and List, never LoomTask*: {offsets}"
    );
}

const DYNAMIC_TASK_JOIN_SOURCE: &str = r#"async fn managed(value Text) Text {
    value.concat("!")
}

async fn failedText() Text {
    assert false
    "unreachable"
}

async fn number(value Int) Int { value }

async fn failedNumber() Int {
    assert false
    0
}

pub async fn main() {
    let pressure = "__PRESSURE__"

    var allTasks = List[Task[Text]]()
    allTasks.add(managed(pressure))
    allTasks.add(managed(pressure))
    let allValues = Task.all(allTasks).await
    let allCount = allValues.length()
    assert allCount == 2

    var anyTasks = List[Task[Int]]()
    anyTasks.add(number(1))
    anyTasks.add(number(2))
    discard Task.any(anyTasks).await

    var settledTasks = List[Task[Text]]()
    settledTasks.add(managed(pressure))
    settledTasks.add(failedText())
    settledTasks.add(failedText())
    let outcomes = Task.settled(settledTasks).await
    let outcomeCount = outcomes.length()
    assert outcomeCount == 3

    var raceTasks = List[Task[Int]]()
    raceTasks.add(number(3))
    raceTasks.add(number(4))
    discard Task.race(raceTasks).await

    let emptyAll = Task.all(List[Task[Int]]()).await
    let emptyAllCount = emptyAll.length()
    assert emptyAllCount == 0
    let emptySettled = Task.settled(List[Task[Int]]()).await
    let emptySettledCount = emptySettled.length()
    assert emptySettledCount == 0
}

pub async fn emptyAny() {
    discard Task.any(List[Task[Int]]()).await
}

pub async fn emptyRace() {
    discard Task.race(List[Task[Int]]()).await
}

pub async fn allFailed() {
    var tasks = List[Task[Int]]()
    tasks.add(failedNumber())
    tasks.add(failedNumber())
    discard Task.any(tasks).await
}

pub async fn raceOrigins() {
    discard Task.race(List[Task[Int]]()).await
    discard Task.race(List[Task[Int]]()).await
}
"#;

fn dynamic_task_join_program() -> CheckedProgram {
    compile_source(&DYNAMIC_TASK_JOIN_SOURCE.replace("__PRESSURE__", &"x".repeat(40 * 1024)))
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one object gate pins the exact dynamic frame roots, direct adoption surface, producer-origin identity, and both cross targets"
)]
fn typed_dynamic_task_join_lists_emit_exact_rooted_objects_for_linux_and_msvc() {
    let program = dynamic_task_join_program();
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("dynamic Task join main instance");
    assert!(main.instructions().iter().any(|instruction| matches!(
        instruction.kind(),
        InstructionKind::TaskJoinList {
            mode: loom_codegen_ir::AwaitMode::All,
            ..
        }
    )));
    let task_list = artifact
        .representations()
        .type_id(&Type::List(Box::new(Type::Task(Box::new(Type::Int)))))
        .expect("exact List[Task[Int]] type");

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create dynamic join target output");
        let object = directory.path().join(if target.contains("windows") {
            "dynamic-task-joins.obj"
        } else {
            "dynamic-task-joins.o"
        });
        let ir_path = directory.path().join("dynamic-task-joins.ll");
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                emit_ir: Some(ir_path.clone()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit dynamic Task joins for {target}: {error}"));
        assert!(object.is_file(), "missing dynamic join object for {target}");
        let ir = std::fs::read_to_string(ir_path).expect("read dynamic Task join IR");
        for required in [
            "loom.lcir.task_join_list.all.",
            "loom.lcir.task_join_list.any.",
            "loom.lcir.task_join_list.settled.",
            "loom.lcir.task_join_list.race.",
            "loom_typed_task_publish_v1",
            "loom_typed_task_publish_adopting_v1",
            "task.join.dynamic.publish.children",
            "task.join.dynamic.register.source.reload",
            "task.join.dynamic.result.source.reload",
            "task.join.dynamic.result.partial.reload",
            "task.join.dynamic.collecting_root_state",
            "loom_gc_typed_repeated_alloc_v1",
            EMPTY_TASK_JOIN_FAULT_CODE,
        ] {
            assert!(
                ir.contains(required),
                "{target} omitted `{required}`:\n{ir}"
            );
        }
        assert!(
            ir.lines().any(|line| {
                line.starts_with("@loom.lcir.task_join_list.all.")
                    && line.contains(".root_offsets =")
                    && line.contains("[2 x i64] [i64 8, i64 16]")
            }),
            "dynamic all frame must expose only source and exact result roots:\n{ir}"
        );
        assert!(
            ir.lines().any(|line| {
                line.starts_with("@loom.lcir.task_join_list.all.")
                    && line.contains(".live_bitmaps =")
                    && line.contains("[4 x i64] [i64 1, i64 1, i64 3, i64 2]")
            }),
            "dynamic all states must root source/source/source+result/result:\n{ir}"
        );
        let task_list_descriptor = ir
            .lines()
            .find(|line| {
                line.starts_with(&format!("@loom.lcir.list.descriptor.{} =", task_list.raw()))
            })
            .expect("List[Task[Int]] repeated descriptor");
        assert!(
            task_list_descriptor.contains("i64 8, i64 0, ptr null"),
            "Task handles must be stable untraced repeated elements: {task_list_descriptor}"
        );
        for forbidden in [
            "%loom.Value",
            "loom.Value",
            "loom_join_",
            "task.join.children",
        ] {
            assert!(
                !ir.contains(forbidden),
                "{target} retained `{forbidden}`:\n{ir}"
            );
        }
        assert!(
            ir.contains(&format!("target triple = \"{target}\"")),
            "{ir}"
        );
    }

    let origins = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "raceOrigins".into(),
        },
    );
    let directory = tempfile::tempdir().expect("create race-origin output");
    let object = directory.path().join("race-origins.o");
    let ir_path = directory.path().join("race-origins.ll");
    emit_lcir_native_object(
        &origins,
        &object,
        &NativeObjectOptions {
            emit_ir: Some(ir_path.clone()),
            ..NativeObjectOptions::default()
        },
    )
    .expect("emit race-origin-sensitive callbacks");
    let ir = std::fs::read_to_string(ir_path).expect("read race-origin IR");
    assert_eq!(
        ir.lines()
            .filter(|line| {
                line.starts_with("@loom.lcir.task_join_list.race.")
                    && line.contains(".descriptor =")
            })
            .count(),
        2,
        "each empty-race producer origin needs its own fault callback:\n{ir}"
    );
}

#[test]
fn typed_dynamic_task_join_lists_run_and_match_canonical_empty_faults() {
    let program = dynamic_task_join_program();
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let native = emit_and_run_lcir(&artifact, "source-dynamic-task-joins");
    assert!(native.output.status.success(), "{:?}", native.output);
    assert_eq!(native.output.stdout, b"Unit\n");

    for entry in ["emptyAny", "emptyRace", "allFailed"] {
        let expected = serde_json::to_value(
            interpret_run(&program, entry).expect_err("dynamic Task join must fault"),
        )
        .expect("serialize interpreter Task join fault");
        let artifact = lower_source_artifact(
            &program,
            &SourceArtifactRequest::Run {
                entry: entry.into(),
            },
        );
        let native = emit_and_run_lcir_machine_fault(
            &artifact,
            &format!("source-dynamic-task-join-{entry}"),
        );
        assert!(
            !native.output.status.success(),
            "{entry}: {:?}",
            native.output
        );
        assert_eq!(machine_fault(&native.output), expected, "{entry}");
        if entry == "allFailed" {
            assert_eq!(expected["fault"]["code"], TASK_ANY_FAILED_FAULT_CODE);
            assert_eq!(expected["fault"]["message"], TASK_ANY_FAILED_FAULT_MESSAGE);
        } else {
            assert_eq!(expected["fault"]["code"], EMPTY_TASK_JOIN_FAULT_CODE);
            assert_eq!(expected["fault"]["message"], EMPTY_TASK_JOIN_FAULT_MESSAGE);
        }
    }
}

#[test]
fn typed_first_class_task_all_propagates_fault_and_cancels_siblings() {
    let source = r#"fn increment(value Int) Int { value + 1 }

async fn overflowChild() Int { increment(9223372036854775807) }

async fn linger(depth Int) Text {
    if depth > 0 {
        linger(depth - 1).await
    } else {
        "finished"
    }
}

pub async fn main() {
    let combined = Task.all(overflowChild(), linger(8))
    let failed, lingered = combined.await
    discard failed
    discard lingered
}
"#;
    let program = compile_source(source);
    let expected = serde_json::to_value(
        interpret_run(&program, "main").expect_err("Task.all child must fault"),
    )
    .expect("serialize interpreter Task.all fault");
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let lcir = emit_and_run_lcir_machine_fault(&artifact, "lcir-task-all-fault");
    let checked_mir =
        emit_and_run_checked_mir_machine_fault(&program, "main", "checked-mir-task-all-fault");
    assert!(!lcir.output.status.success(), "{:?}", lcir.output);
    assert!(!checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(machine_fault(&lcir.output), expected, "LCIR Task.all fault");
    assert_eq!(
        machine_fault(&checked_mir),
        expected,
        "checked-MIR Task.all fault"
    );
    assert!(lcir.ir.contains("ret i32 2"), "{}", lcir.ir);
    assert!(lcir.ir.contains("ret i32 3"), "{}", lcir.ir);
    assert!(
        !diagnostic_text(&lcir.output).contains("LOOM_RUNTIME_TYPED_CANCEL_UNREQUESTED"),
        "Task.all sibling cancellation was classified as a callback defect: {:?}",
        lcir.output
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential gate keeps fallible coroutine effects, ordinary Result values, exact child-fault inheritance, root lifecycles, and cross-target objects together"
)]
fn fallible_typed_async_results_and_faults_close_the_native_route() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-fallible-async/main.loom"),
        include_str!("../../../fixtures/lcir-fallible-async/main_test.loom"),
    );
    assert_eq!(interpret_run(&program, "main"), Ok(Value::Unit));
    for policy in [NativeRoutePolicy::Automatic, NativeRoutePolicy::LcirOnly] {
        let prepared = prepare_native_object(&program, EmitOptions::run("main"), policy)
            .expect("prepare fallible typed-async route");
        assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    }

    let unsupported = lower_typed_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(32).expect("32-bit target layout"),
    )
    .expect("classify fallible typed async for 32-bit target");
    assert!(
        matches!(unsupported, LoweringOutcome::Unsupported(_)),
        "typed Tasks must fail closed outside the pinned 64-bit ABI: {unsupported:?}"
    );

    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let checked_answer = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("checkedAnswer"))
        .expect("checkedAnswer coroutine");
    let outcome = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("outcome"))
        .expect("managed Result coroutine");
    let verify = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("verify"))
        .expect("verify coroutine");
    assert!(checked_answer.coroutine().is_some());
    assert!(checked_answer.effects().contains(Effects::MAY_FAULT));
    assert!(outcome.effects().contains(Effects::MAY_COLLECT));
    let outcome_plan = outcome.coroutine().expect("managed Result coroutine plan");
    assert!(outcome_plan.suspensions().is_empty());
    assert!(matches!(
        artifact
            .representations()
            .value_type(outcome_plan.output())
            .and_then(|value| artifact.representations().repr(value.repr())),
        Some(Repr::Sum(_))
    ));
    let verify_plan = verify.coroutine().expect("verify coroutine plan");
    assert_eq!(
        verify_plan
            .suspensions()
            .iter()
            .map(loom_codegen_ir::CoroutineSuspension::state)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(verify.effects().contains(Effects::MAY_FAULT));
    let final_live_reprs = verify_plan.suspensions()[3]
        .live()
        .iter()
        .filter_map(|ty| {
            artifact
                .representations()
                .value_type(*ty)
                .and_then(|value| artifact.representations().repr(value.repr()))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        final_live_reprs
            .iter()
            .filter(|repr| matches!(repr, Repr::Sum(_)))
            .count(),
        2,
        "both completed Result values must stay live across allocation pressure"
    );
    assert_eq!(
        final_live_reprs
            .iter()
            .filter(|repr| ***repr == Repr::ManagedPointer)
            .count(),
        1,
        "the source Text must stay live beside both Result carriers"
    );

    let dump = dump_program(artifact.program());
    for required in [
        "effects=may_fault+needs_runtime+may_collect+needs_executor coroutine",
        "invoke",
        "contract PreconditionFault",
        "contract PostconditionFault",
        "resume_fault",
        "await_tasks all state",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }

    let lcir = emit_and_run_lcir(&artifact, "source-fallible-typed-async");
    let checked_mir =
        emit_and_run_checked_mir(&program, "main", "checked-mir-fallible-typed-async");
    assert!(lcir.output.status.success(), "{:?}", lcir.output);
    assert!(checked_mir.status.success(), "{checked_mir:?}");
    assert_eq!(lcir.output.stdout, b"Unit\n");
    assert_eq!(lcir.output.stdout, checked_mir.stdout);
    assert_eq!(lcir.output.stderr, checked_mir.stderr);
    for required in [
        "%loom.lcir.FaultContext = type",
        "fault.context.runtime.pointer",
        "loom_context_raise_fault_v1",
        "loom_task_report_fault",
        "task.await.faulted",
        "task.await.cancelled",
        "loom_typed_task_publish_result_v1",
        "loom_typed_task_take_result_v1",
    ] {
        assert!(
            lcir.ir.contains(required),
            "missing `{required}`:\n{}",
            lcir.ir
        );
    }
    let outcome_offsets = format!(
        "@loom.lcir.coroutine.root_offsets.{} = private unnamed_addr constant [2 x i64] [i64 8, i64 32]",
        outcome.id().raw()
    );
    let outcome_bitmaps = format!(
        "@loom.lcir.coroutine.live_bitmaps.{} = private unnamed_addr constant [2 x i64] [i64 1, i64 2]",
        outcome.id().raw()
    );
    assert!(
        lcir.ir.contains(&outcome_offsets),
        "managed Result frame must expose exactly the parameter root at offset 8 and completed carrier root at offset 32:\n{}",
        lcir.ir
    );
    assert!(
        lcir.ir.contains(&outcome_bitmaps),
        "managed Result frame must switch exactly from its parameter root to its completed carrier root:\n{}",
        lcir.ir
    );
    let verify_offsets = format!(
        "@loom.lcir.coroutine.root_offsets.{} = private unnamed_addr constant [9 x i64] [i64 16, i64 48, i64 72, i64 88, i64 104, i64 120, i64 136, i64 152, i64 168]",
        verify.id().raw()
    );
    let verify_bitmaps = format!(
        "@loom.lcir.coroutine.live_bitmaps.{} = private unnamed_addr constant [6 x i64] [i64 0, i64 1, i64 6, i64 56, i64 448, i64 0]",
        verify.id().raw()
    );
    assert!(
        lcir.ir.contains(&verify_offsets),
        "parent frame managed-root offsets changed unexpectedly:\n{}",
        lcir.ir
    );
    assert!(
        lcir.ir.contains(&verify_bitmaps),
        "the fourth suspension must expose exactly one Text and two Result carrier roots:\n{}",
        lcir.ir
    );
    let outcome_callback_name = format!("@loom.lcir.coroutine.resume.{}(", outcome.id().raw());
    let outcome_callback = lcir
        .ir
        .split("\ndefine ")
        .find(|function| function.contains(&outcome_callback_name))
        .expect("managed Result coroutine callback IR");
    for required in [
        "managed.root.sum.variant.active",
        "managed.root.active.pointer = select",
        "managed.root.sum.safe.carrier",
        "managed.root.rebuild.sum.safe.carrier",
        "managed.root.rebuild.active.sum",
        "zeroinitializer",
    ] {
        assert!(
            outcome_callback.contains(required),
            "managed Result callback omitted `{required}`:\n{outcome_callback}"
        );
    }
    let mut rooted_callbacks = 0_usize;
    for function in lcir.ir.split("\ndefine ").filter(|function| {
        function.contains("loom.lcir.coroutine.resume.")
            && function.contains("loom_gc_typed_root_push_v1")
    }) {
        rooted_callbacks += 1;
        let returns = function
            .lines()
            .filter(|line| line.trim_start().starts_with("ret i32 "))
            .count();
        let pops = function
            .matches("call i32 @loom_gc_typed_root_pop_v1")
            .count();
        assert!(
            returns > 0,
            "rooted coroutine has no terminal return:\n{function}"
        );
        assert_eq!(
            pops, returns,
            "every rooted coroutine callback exit must pop exactly once:\n{function}"
        );
    }
    assert!(
        rooted_callbacks > 0,
        "fixture emitted no rooted callback:\n{}",
        lcir.ir
    );

    let interpreted = Interpreter::new(&program).run_tests();
    assert_eq!(interpreted.len(), 1, "{interpreted:#?}");
    assert_eq!(
        interpreted[0].status,
        TestStatus::Passed,
        "{interpreted:#?}"
    );
    let test_artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let native_tests = emit_and_run_lcir(&test_artifact, "source-fallible-typed-async-tests");
    let checked_mir_tests =
        emit_and_run_checked_mir_tests(&program, "checked-mir-fallible-typed-async-tests");
    assert!(
        native_tests.output.status.success(),
        "{:?}",
        native_tests.output
    );
    assert!(checked_mir_tests.status.success(), "{checked_mir_tests:?}");
    assert_eq!(native_tests.output.stdout, checked_mir_tests.stdout);
    assert_eq!(native_tests.output.stderr, checked_mir_tests.stderr);

    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let directory = tempfile::tempdir().expect("create fallible async target output");
        let object = directory.path().join(if target.contains("windows") {
            "fallible-async.obj"
        } else {
            "fallible-async.o"
        });
        emit_lcir_native_object(
            &artifact,
            &object,
            &NativeObjectOptions {
                target_triple: Some(target.to_owned()),
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit fallible async object for {target}: {error}"));
        assert!(
            object.is_file(),
            "missing fallible async object for {target}"
        );
    }

    let fault_source = r"fn increment(value Int) Int { value + 1 }

async fn overflowChild() Int { increment(9223372036854775807) }

async fn linger(depth Int) Int {
    if depth > 0 {
        linger(depth - 1).await
    } else {
        0
    }
}

pub async fn runtimeMain() {
    let failed = overflowChild()
    let sibling = linger(8)
    discard failed.await
    discard sibling.await
}

async fn assertionChild() {
    assert false
}

pub async fn assertionMain() {
    assertionChild().await
}

async fn required(value Int)
    requires value > 0
{
}

async fn preconditionParent() {
    required(0).await
}

pub async fn preconditionMain() {
    preconditionParent().await
}

async fn wrongAnswer() Int
    ensures result == 42
{
    41
}

pub async fn postconditionMain() {
    discard wrongAnswer().await
}

";
    let fault_program = compile_source(fault_source);
    for entry in [
        "runtimeMain",
        "assertionMain",
        "preconditionMain",
        "postconditionMain",
    ] {
        let expected = serde_json::to_value(
            interpret_run(&fault_program, entry).expect_err("async child must fault"),
        )
        .expect("serialize interpreter async fault");
        let artifact = lower_source_artifact(
            &fault_program,
            &SourceArtifactRequest::Run {
                entry: entry.into(),
            },
        );
        if entry == "runtimeMain" {
            let parent = artifact
                .functions()
                .iter()
                .find(|function| function.name().ends_with("runtimeMain"))
                .expect("runtime fault parent coroutine");
            let plan = parent.coroutine().expect("runtime fault coroutine plan");
            assert_eq!(plan.suspensions().len(), 2);
            assert_eq!(
                plan.suspensions()[0]
                    .live()
                    .iter()
                    .filter(|ty| {
                        artifact
                            .representations()
                            .value_type(**ty)
                            .and_then(|value| artifact.representations().repr(value.repr()))
                            == Some(&Repr::TaskHandle)
                    })
                    .count(),
                1,
                "the pending sibling Task must remain in the parent's first suspension row"
            );
        }
        let lcir =
            emit_and_run_lcir_machine_fault(&artifact, &format!("lcir-fallible-async-{entry}"));
        let checked_mir = emit_and_run_checked_mir_machine_fault(
            &fault_program,
            entry,
            &format!("checked-mir-fallible-async-{entry}"),
        );
        assert!(!lcir.output.status.success(), "{entry}: {:?}", lcir.output);
        assert!(!checked_mir.status.success(), "{entry}: {checked_mir:?}");
        assert_eq!(machine_fault(&lcir.output), expected, "LCIR {entry}");
        assert_eq!(machine_fault(&checked_mir), expected, "checked-MIR {entry}");
        assert!(
            lcir.ir.contains("loom_task_report_fault"),
            "{entry}: {}",
            lcir.ir
        );
        assert!(lcir.ir.contains("ret i32 2"), "{entry}: {}", lcir.ir);
        assert!(lcir.ir.contains("ret i32 3"), "{entry}: {}", lcir.ir);
        if entry == "runtimeMain" {
            let linger = artifact
                .functions()
                .iter()
                .find(|function| function.name().ends_with("linger"))
                .expect("sibling coroutine");
            let descriptor_name =
                format!("@loom.lcir.coroutine.descriptor.{} =", linger.id().raw());
            let descriptor = lcir
                .ir
                .lines()
                .find(|line| line.starts_with(&descriptor_name))
                .expect("sibling coroutine descriptor");
            let callback_name = format!("@loom.lcir.coroutine.resume.{}", linger.id().raw());
            assert!(
                descriptor.matches(&callback_name).count() == 2,
                "pending sibling must use its checked resume callback for both resume and cancellation: {descriptor}"
            );
            let cancel_callback = lcir
                .ir
                .split("\ndefine ")
                .find(|function| function.contains(&format!("{callback_name}(")))
                .expect("typed coroutine resume-and-cancellation callback");
            assert_eq!(
                cancel_callback.matches("ret i32 3").count(),
                2,
                "typed sibling cancellation must terminate both the never-started and suspended states:\n{cancel_callback}"
            );
            assert!(
                cancel_callback.contains("coroutine.cancel.dispatch")
                    && cancel_callback.contains("loom_typed_task_is_cancel_requested_v1"),
                "typed sibling callback omitted checked cancellation dispatch:\n{cancel_callback}"
            );
            assert!(
                !diagnostic_text(&lcir.output).contains("LOOM_RUNTIME_TYPED_CANCEL_UNREQUESTED"),
                "sibling cancellation was misclassified as a callback defect: {:?}",
                lcir.output
            );
        }
    }
}
