#![allow(clippy::default_trait_access)]

use std::collections::BTreeSet;
use std::process::{Command, Output};

use loom_codegen_ir::{
    CheckedArtifact, Effects, InstanceRole, LoweringOutcome, Repr, SourceArtifactRequest,
    TargetLayout, dump_program, lower_typed_artifact,
};
use loom_codegen_llvm::{
    DebugSource, NativeObjectOptions, OptimizationProfile, emit_lcir_native_object,
};
use loom_mir::CheckedProgram;
use loom_runtime_abi::{FAULT_FORMAT_ENV, FAULT_FORMAT_JSON, FAULT_JSON_PREFIX};

mod support;
use support::link_native_object;

struct NativeRun {
    ir: String,
    output: Output,
}

fn analyze_sources(source: &str, test_source: Option<&str>) -> loom_driver::AnalysisSnapshot {
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

fn compile_source(source: &str) -> CheckedProgram {
    analyze_sources(source, None)
        .executable()
        .expect("lower checked MIR")
        .clone()
}

fn compile_sources(source: &str, test_source: &str) -> CheckedProgram {
    analyze_sources(source, Some(test_source))
        .executable()
        .expect("lower checked MIR")
        .clone()
}

fn compile_source_with_debug_sources(source: &str) -> (CheckedProgram, Vec<DebugSource>) {
    let snapshot = analyze_sources(source, None);
    let debug_sources = snapshot
        .sources()
        .documents()
        .iter()
        .map(|document| {
            DebugSource::new(
                document.id().0,
                document.relative_path(),
                document.text().expect("checked source must be UTF-8"),
            )
        })
        .collect();
    let program = snapshot.executable().expect("lower checked MIR").clone();
    (program, debug_sources)
}

fn lower_source_artifact(
    program: &CheckedProgram,
    request: &SourceArtifactRequest,
) -> CheckedArtifact {
    let layout =
        TargetLayout::new(u16::try_from(usize::BITS).expect("host pointer width fits u16"))
            .expect("supported host target layout");
    match lower_typed_artifact(program, request, layout).expect("lower typed LCIR") {
        LoweringOutcome::Complete(artifact) => artifact,
        LoweringOutcome::Unsupported(report) => {
            panic!("source fixture unexpectedly unsupported: {report:#?}")
        }
    }
}

fn emit_and_run(artifact: &CheckedArtifact, stem: &str) -> NativeRun {
    emit_and_run_with_options(artifact, stem, NativeObjectOptions::default(), false)
}

fn emit_and_run_machine_fault(artifact: &CheckedArtifact, stem: &str) -> NativeRun {
    emit_and_run_with_options(artifact, stem, NativeObjectOptions::default(), true)
}

fn emit_and_run_with_options(
    artifact: &CheckedArtifact,
    stem: &str,
    mut options: NativeObjectOptions,
    machine_faults: bool,
) -> NativeRun {
    let directory = tempfile::tempdir().expect("create native output directory");
    let object = directory.path().join(format!("{stem}.o"));
    let ir_path = directory.path().join(format!("{stem}.ll"));
    let executable = directory.path().join(stem);
    options.emit_ir = Some(ir_path.clone());
    emit_lcir_native_object(artifact, &object, &options).expect("emit typed LCIR object");
    link_native_object(&object, &executable).expect("link typed LCIR object");
    let mut command = Command::new(executable);
    if machine_faults {
        command.env(FAULT_FORMAT_ENV, FAULT_FORMAT_JSON);
    }
    let output = command.output().expect("run typed LCIR executable");
    NativeRun {
        ir: std::fs::read_to_string(ir_path).expect("read emitted LLVM IR"),
        output,
    }
}

fn assert_success(run: &NativeRun) {
    assert!(run.output.status.success(), "{:?}", run.output);
    assert!(run.output.stderr.is_empty(), "{:?}", run.output);
}

fn assert_typed_lcir_surface(ir: &str) {
    assert!(
        ir.contains("@loom.lcir."),
        "missing typed LCIR symbols:\n{ir}"
    );
    for forbidden in [
        "%loom.Value",
        "ArgNode",
        "ValueNode",
        "@loom.fn.",
        "loom_gc_root_push_v1",
        "loom_gc_root_pop_v1",
        "loom_witness_",
        "WitnessInstance",
    ] {
        assert!(
            !ir.contains(forbidden),
            "legacy backend token `{forbidden}` remained:\n{ir}"
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
            "indirect LLVM call in closed-world LCIR:\n{line}\n\n{ir}"
        );
    }
}

fn machine_fault(output: &Output) -> serde_json::Value {
    let stderr = String::from_utf8(output.stderr.clone()).expect("machine fault is UTF-8");
    let json = stderr
        .lines()
        .find_map(|line| line.strip_prefix(FAULT_JSON_PREFIX))
        .unwrap_or_else(|| panic!("missing machine fault in `{stderr}`"));
    serde_json::from_str(json).expect("machine fault is valid JSON")
}

fn emit_cross_target_objects(artifact: &CheckedArtifact, stem: &str) {
    for (target, extension, magic) in [
        (
            "x86_64-unknown-linux-gnu",
            "o",
            &[0x7f, b'E', b'L', b'F'][..],
        ),
        ("x86_64-pc-windows-msvc", "obj", &[0x64, 0x86][..]),
    ] {
        let directory = tempfile::tempdir().expect("create cross-target output directory");
        let object = directory.path().join(format!("{stem}.{extension}"));
        let ir_path = directory.path().join(format!("{stem}.ll"));
        emit_lcir_native_object(
            artifact,
            &object,
            &NativeObjectOptions {
                emit_ir: Some(ir_path.clone()),
                target_triple: Some(target.to_owned()),
                optimization: OptimizationProfile::Release,
                ..NativeObjectOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("emit `{stem}` for {target}: {error}"));
        let bytes = std::fs::read(&object).expect("read cross-target object");
        assert_eq!(bytes.get(..magic.len()), Some(magic), "{target}");
        let ir = std::fs::read_to_string(ir_path).expect("read cross-target LLVM IR");
        assert!(
            ir.contains(&format!("target triple = \"{target}\"")),
            "{ir}"
        );
        assert_typed_lcir_surface(&ir);
    }
}

#[test]
fn source_products_publish_exact_debug_abis_for_direct_and_fallible_inout() {
    let source = r"record Counter {
    value Int
    enabled Bool
}

impl Counter {
    method reset(mut self, value Int) {
        self.value = value
    }

    method checkedAdd(mut self, value Int) {
        self.value = self.value + value
        assert self.value >= 0
    }
}

pub fn main() {
    var counter = Counter { value = 1, enabled = true }
    counter.reset(2)
    counter.checkedAdd(3)
}
";
    let (program, debug_sources) = compile_source_with_debug_sources(source);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let run = emit_and_run_with_options(
        &artifact,
        "source-product-debug",
        NativeObjectOptions::default().with_debug_sources(debug_sources),
        false,
    );
    assert_success(&run);
    for required in [
        "name: \"LoomProduct<",
        "name: \"LoomInOut<",
        "name: \"LoomFallibleInOut<",
        "name: \"writeback0\"",
        "name: \"field1\"",
        "offset: 64",
        "DIFlagArtificial",
    ] {
        assert!(
            run.ir.contains(required),
            "missing `{required}`:\n{}",
            run.ir
        );
    }
    assert_typed_lcir_surface(&run.ir);
}

#[test]
fn release_tuples_and_sums_stay_in_register_ssa() {
    let source = r"record Packet { pair (Int, Bool) }

enum Choice {
    Empty
    PacketValue(Packet)
    Flags(Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool)
}

fn roundTrip(input (Packet, Float)) (Bool, Packet) {
    let packet, ignored = input
    discard ignored
    let number, enabled = packet.pair
    (enabled, Packet { pair = (number, enabled) })
}

fn score(value Choice) Int {
    match value {
        Empty => 0
        PacketValue(packet) => {
            let number, enabled = packet.pair
            if enabled { number } else { 0 }
        }
        Flags(_, _, _, _, _, _, _, _, last) => if last { 9 } else { 8 }
    }
}

pub fn main() {
    let enabled, packet = roundTrip((Packet { pair = (40, true) }, 1.5))
    let packetScore = score(Choice.PacketValue(packet))
    let flagScore = score(Choice.Flags(false, false, false, false, false, false, false, false, true))
    assert enabled
    assert packetScore == 40
    assert flagScore == 9
}
";
    let program = compile_source(source);
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let run = emit_and_run_with_options(
        &artifact,
        "release-source-aggregates",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
        false,
    );
    assert_success(&run);
    for forbidden in ["alloca", "memcpy", "loom_gc_", "loom_executor_"] {
        assert!(
            !run.ir.contains(forbidden),
            "release aggregate retained `{forbidden}`:\n{}",
            run.ir
        );
    }
    assert_no_indirect_calls(&run.ir);
    assert_typed_lcir_surface(&run.ir);
}

#[test]
fn projected_places_run_as_functional_product_updates() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-projected-places/main.loom"),
        include_str!("../../../fixtures/lcir-projected-places/main_test.loom"),
    );
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("product.extract"), "{dump}");
    assert!(dump.contains("product.insert"), "{dump}");
    assert!(dump.contains("inout=[0]"), "{dump}");

    let run = emit_and_run(&artifact, "source-projected-places");
    assert_success(&run);
    assert_no_indirect_calls(&run.ir);
    assert_typed_lcir_surface(&run.ir);
}

#[test]
fn managed_products_sums_and_lists_use_precise_typed_gc_metadata() {
    let cases = [
        (
            "managed-products",
            include_str!("../../../fixtures/lcir-managed-products/main.loom"),
            include_str!("../../../fixtures/lcir-managed-products/main_test.loom"),
            &["managed.root.rebuild", "loom_gc_typed_root_push_v1"][..],
        ),
        (
            "managed-sums",
            include_str!("../../../fixtures/lcir-managed-sums/main.loom"),
            include_str!("../../../fixtures/lcir-managed-sums/main_test.loom"),
            &[
                "managed.root.sum.variant.active",
                "managed.root.rebuild.active.sum",
            ][..],
        ),
        (
            "managed-lists",
            include_str!("../../../fixtures/lcir-managed-lists/main.loom"),
            include_str!("../../../fixtures/lcir-managed-lists/main_test.loom"),
            &[
                "loom.lcir.list.descriptor",
                "loom.lcir.list.pointer_offsets",
                "loom_gc_typed_repeated_alloc_v1",
            ][..],
        ),
    ];

    for (stem, source, test_source, required) in cases {
        let program = compile_sources(source, test_source);
        let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
        let run = emit_and_run(&artifact, stem);
        assert_success(&run);
        assert!(
            String::from_utf8_lossy(&run.output.stdout).contains("passed "),
            "{:?}",
            run.output
        );
        for token in required {
            assert!(
                run.ir.contains(token),
                "{stem} omitted `{token}`:\n{}",
                run.ir
            );
        }
        assert_typed_lcir_surface(&run.ir);
    }
}

#[test]
fn interleaved_sum_layouts_keep_pointer_and_scalar_bytes_disjoint() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-sum-layout-collisions/main.loom"),
        include_str!("../../../fixtures/lcir-sum-layout-collisions/main_test.loom"),
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let run = emit_and_run(&artifact, "source-sum-layout-collisions");
    assert_success(&run);
    assert!(
        String::from_utf8_lossy(&run.output.stdout)
            .contains("passed standalone.sumLayoutCollisions"),
        "{:?}",
        run.output
    );

    for required in [
        "[2 x i64] [i64 8, i64 24]",
        "i64 32, i64 2, ptr @loom.lcir.list.pointer_offsets",
        "managed.root.rebuild.active.sum",
    ] {
        assert!(
            run.ir.contains(required),
            "sum collision layout omitted `{required}`:\n{}",
            run.ir
        );
    }
    assert_typed_lcir_surface(&run.ir);
    emit_cross_target_objects(&artifact, "sum-layout-collisions");
}

#[test]
fn recursive_structural_equality_uses_a_closed_typed_helper_cycle() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-recursive-equality/main.loom"),
        include_str!("../../../fixtures/lcir-recursive-equality/main_test.loom"),
    );
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
    assert_eq!(helpers.len(), 9, "{}", dump_program(artifact.program()));
    assert!(
        helpers
            .iter()
            .all(|helper| helper.effects() == Effects::NONE)
    );
    let helper_ids = helpers
        .iter()
        .map(|helper| helper.id())
        .collect::<BTreeSet<_>>();
    assert!(
        helpers
            .iter()
            .flat_map(|helper| helper.instructions())
            .any(|instruction| matches!(
                instruction.kind(),
                loom_codegen_ir::InstructionKind::DirectCall { callee, .. }
                    if helper_ids.contains(callee)
            ))
    );

    let run = emit_and_run_with_options(
        &artifact,
        "source-recursive-equality",
        NativeObjectOptions::default().with_optimization(OptimizationProfile::Release),
        false,
    );
    assert_success(&run);
    assert!(!run.ir.contains("loom_runtime_json_equal"), "{}", run.ir);
    assert_no_indirect_calls(&run.ir);
    assert_typed_lcir_surface(&run.ir);
}

#[test]
fn static_concepts_are_specialized_without_runtime_witnesses() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-static-concepts/main.loom"),
        include_str!("../../../fixtures/lcir-static-concepts/main_test.loom"),
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let dump = dump_program(artifact.program());
    assert!(dump.contains("witnesses=[Apply#"), "{dump}");
    assert!(dump.contains("witnesses=[Concrete#"), "{dump}");
    assert!(!dump.contains("Projection#"), "{dump}");

    let run = emit_and_run(&artifact, "source-static-concepts");
    assert_success(&run);
    assert!(!run.ir.contains("loom_gc_"), "{}", run.ir);
    assert!(!run.ir.contains("loom_executor_"), "{}", run.ir);
    assert_no_indirect_calls(&run.ir);
    assert_typed_lcir_surface(&run.ir);
}

#[test]
fn finite_dynamic_concepts_use_precise_boxes_and_closed_direct_dispatch() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-dyn-finite/main.loom"),
        include_str!("../../../fixtures/lcir-dyn-finite/main_test.loom"),
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    assert_eq!(artifact.representations().dynamics().len(), 2);
    for dynamic in artifact.representations().dynamics() {
        assert_eq!(dynamic.candidates().len(), 2);
        assert_eq!(
            artifact
                .representations()
                .value_type(dynamic.view())
                .and_then(|ty| artifact.representations().repr(ty.repr())),
            Some(&Repr::ManagedPointer)
        );
    }
    let dump = dump_program(artifact.program());
    for required in ["dyn.construct", "dyn.switch", "inout=[0]"] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    for dead in ["standalone.cold", "7001", "7002"] {
        assert!(!dump.contains(dead), "retained `{dead}`:\n{dump}");
    }

    let run = emit_and_run(&artifact, "source-finite-dyn");
    assert_success(&run);
    for required in [
        "loom.lcir.dyn.descriptor.",
        "loom.lcir.dyn.pointer_offsets.",
        "loom_gc_typed_alloc_v1",
        "switch i32",
    ] {
        assert!(
            run.ir.contains(required),
            "missing `{required}`:\n{}",
            run.ir
        );
    }
    assert_no_indirect_calls(&run.ir);
    assert_typed_lcir_surface(&run.ir);
}

#[test]
fn generic_products_and_refinements_keep_their_typed_physical_values() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-generic-products/main.loom"),
        include_str!("../../../fixtures/lcir-generic-products/main_test.loom"),
    );
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("standalone.swap"), "{dump}");
    assert!(dump.contains("refine.proven"), "{dump}");
    assert!(dump.contains("invariant_record.proven"), "{dump}");

    let run = emit_and_run(&artifact, "source-generic-products");
    assert_success(&run);
    assert!(run.ir.contains("managed.root.reload"), "{}", run.ir);
    assert_no_indirect_calls(&run.ir);
    assert_typed_lcir_surface(&run.ir);
}

#[test]
fn lexical_defer_and_scoped_disposal_execute_at_each_block_boundary() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-lexical-cleanup/main.loom"),
        include_str!("../../../fixtures/lcir-lexical-cleanup/main_test.loom"),
    );
    let run_artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let dump = dump_program(run_artifact.program());
    assert!(dump.contains("standalone.dispose"), "{dump}");
    assert!(dump.contains("standalone.replaceAfterCleanup"), "{dump}");
    let run = emit_and_run(&run_artifact, "source-lexical-cleanup");
    assert_success(&run);
    assert_typed_lcir_surface(&run.ir);

    let tests = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let tests = emit_and_run(&tests, "source-lexical-cleanup-tests");
    assert_success(&tests);
    assert!(
        String::from_utf8_lossy(&tests.output.stdout).contains("passed standalone.lexicalCleanup"),
        "{:?}",
        tests.output
    );
}

#[test]
fn source_test_roots_preserve_declaration_order_in_the_native_harness() {
    let program = compile_sources(
        "",
        r"test fn zeta() {}
test fn alpha() {}
test fn middle() {}
",
    );
    let artifact = lower_source_artifact(&program, &SourceArtifactRequest::Tests);
    let roots = artifact
        .test_roots()
        .expect("test roots")
        .iter()
        .map(|root| artifact.function(*root).expect("test function").name())
        .collect::<Vec<_>>();
    assert_eq!(
        roots,
        ["standalone.zeta", "standalone.alpha", "standalone.middle"]
    );

    let run = emit_and_run(&artifact, "source-test-order");
    assert_success(&run);
    let results = String::from_utf8(run.output.stdout.clone()).expect("UTF-8 test output");
    assert!(
        results.contains(
            "passed standalone.zeta\npassed standalone.alpha\npassed standalone.middle\n"
        ),
        "{results}"
    );
    assert!(!run.ir.contains("loom_executor_"), "{}", run.ir);
    assert_typed_lcir_surface(&run.ir);
}

#[test]
fn async_cleanup_runs_on_normal_fault_and_cancellation_edges() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-async-cleanup/main.loom"),
        include_str!("../../../fixtures/lcir-async-cleanup/main_test.loom"),
    );
    let main = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let run = emit_and_run(&main, "source-async-cleanup");
    assert_success(&run);
    for required in [
        "loom.lcir.coroutine.resume.",
        "loom.lcir.coroutine.descriptor.",
        "coroutine.cancel.dispatch",
        "task.await.fault.live",
        "loom_typed_task_is_cancel_requested_v1",
    ] {
        assert!(
            run.ir.contains(required),
            "missing `{required}`:\n{}",
            run.ir
        );
    }
    assert_typed_lcir_surface(&run.ir);

    for (entry, stem) in [
        ("faultCleanupMain", "source-async-cleanup-fault"),
        ("cancellationMain", "source-async-cleanup-cancel"),
    ] {
        let artifact = lower_source_artifact(
            &program,
            &SourceArtifactRequest::Run {
                entry: entry.into(),
            },
        );
        let failed = emit_and_run_machine_fault(&artifact, stem);
        assert!(
            !failed.output.status.success(),
            "{entry}: {:?}",
            failed.output
        );
        assert_eq!(
            machine_fault(&failed.output)["fault"]["code"],
            "AssertionFault"
        );
        assert_typed_lcir_surface(&failed.ir);
    }
}

#[test]
fn managed_coroutine_frames_publish_precise_roots_on_every_target() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-async-managed-collections/main.loom"),
        include_str!("../../../fixtures/lcir-async-managed-collections/main_test.loom"),
    );
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let run = emit_and_run(&artifact, "source-async-managed-collections");
    assert_success(&run);
    for required in [
        "loom.lcir.coroutine.root_offsets.",
        "loom.lcir.coroutine.live_bitmaps.",
        "loom.lcir.list.descriptor",
        "loom.lcir.text_map.descriptor",
        "loom_gc_typed_repeated_alloc_v1",
        "managed.root.reload",
    ] {
        assert!(
            run.ir.contains(required),
            "missing `{required}`:\n{}",
            run.ir
        );
    }
    assert_typed_lcir_surface(&run.ir);
    emit_cross_target_objects(&artifact, "managed-coroutine");
}

#[test]
fn runtime_width_task_joins_use_typed_descriptors_and_executor_abi() {
    let program = compile_sources(
        include_str!("../../../fixtures/lcir-typed-task-lists/main.loom"),
        include_str!("../../../fixtures/lcir-typed-task-lists/main_test.loom"),
    );
    let artifact = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
    );
    let run = emit_and_run(&artifact, "source-dynamic-task-joins");
    assert_success(&run);
    for required in [
        "loom.lcir.task_join_list.all.",
        "loom.lcir.task_join_list.any.",
        "loom.lcir.task_join_list.settled.",
        "loom.lcir.task_join_list.race.",
        "task.join.dynamic.publish.children",
        "loom_typed_task_publish_adopting_v1",
        "loom_gc_typed_repeated_alloc_v1",
        "loom_executor_run",
    ] {
        assert!(
            run.ir.contains(required),
            "missing `{required}`:\n{}",
            run.ir
        );
    }
    assert_typed_lcir_surface(&run.ir);

    let empty = lower_source_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "emptyAny".into(),
        },
    );
    let empty = emit_and_run_machine_fault(&empty, "source-empty-task-any");
    assert!(!empty.output.status.success(), "{:?}", empty.output);
    assert_eq!(
        machine_fault(&empty.output)["fault"]["code"],
        "EmptyTaskJoin"
    );
}
