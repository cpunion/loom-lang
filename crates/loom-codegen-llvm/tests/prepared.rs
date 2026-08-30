#![allow(clippy::default_trait_access)]

use loom_codegen_ir::UnsupportedFeature;
use loom_codegen_llvm::{
    DebugSource, EmitOptions, NativePreparationErrorKind, NativeRouteKind, NativeRoutePolicy,
    OptimizationProfile, emit_prepared_native_object, prepare_native_object,
    prepared_native_object_fingerprint, prepared_native_target_identity, target_identity,
};

mod support;
use loom_mir::CheckedProgram;

fn compile_source(source: &str) -> CheckedProgram {
    compile_project_sources(source, None)
}

fn compile_sources(source: &str, test_source: &str) -> CheckedProgram {
    compile_project_sources(source, Some(test_source))
}

fn compile_project_sources(source: &str, test_source: Option<&str>) -> CheckedProgram {
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
    snapshot.executable().expect("lower checked MIR").clone()
}

fn scalar_program() -> CheckedProgram {
    compile_source(
        r"fn choose(flag Bool) Int {
    if flag { 7 } else { 9 }
}

pub fn main() {
    discard choose(true)
}
",
    )
}

fn allocating_text_program() -> CheckedProgram {
    compile_source(
        r#"pub fn main() {
    discard "left".concat("right")
}
"#,
    )
}

fn text_get_program() -> CheckedProgram {
    compile_source(
        r#"pub fn main() {
    discard "value".get(0)
}
"#,
    )
}

fn typed_timer_program() -> CheckedProgram {
    compile_source(
        r"pub async fn main() {
    Task.sleep(1).await
}
",
    )
}

fn sync_task_creation_program(nested: bool) -> CheckedProgram {
    let helper = if nested {
        "fn inner() Task[Int] { child() }\n\nfn helper() Task[Int] { inner() }"
    } else {
        "fn helper() Task[Int] { child() }"
    };
    compile_source(&format!(
        r"async fn child() Int {{ 1 }}

{helper}

pub async fn main() {{
    discard helper().await
}}
"
    ))
}

fn sync_executor_root_with_unsupported_site_program() -> CheckedProgram {
    compile_source(
        r"dyn concept Truth { method truth(self) Bool }

fn missing() dyn Truth { missing() }

async fn child() Int { 1 }

fn consume(task Task[Int]) {
    discard missing().truth()
    consume(task)
}

pub fn main() {
    consume(child())
}
",
    )
}

fn sync_executor_root_with_nonregular_instance_program() -> CheckedProgram {
    compile_source(
        r"async fn child() Int { 1 }

fn consume(task Task[Int]) {
    consume(task)
}

fn spiral[T](value T) {
    spiral((value, value))
}

pub fn main() {
    consume(child())
    spiral(0)
}
",
    )
}

fn unsupported_dynamic_program() -> CheckedProgram {
    compile_source(
        r"dyn concept Truth { method truth(self) Bool }

pub fn main() {
    discard missing().truth()
}

fn missing() dyn Truth { missing() }
",
    )
}

#[test]
fn automatic_route_is_atomic_over_the_reachable_artifact() {
    let scalar = scalar_program();
    let prepared = prepare_native_object(
        &scalar,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare typed artifact");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let text = allocating_text_program();
    let prepared = prepare_native_object(
        &text,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare managed Text artifact");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let text_get = text_get_program();
    let prepared = prepare_native_object(
        &text_get,
        EmitOptions::run("main"),
        NativeRoutePolicy::LcirOnly,
    )
    .expect("prepare typed Text.get artifact");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let managed_tuple = compile_source(
        r#"fn make() (Int, Text) { (1, "value") }

pub fn main() {
    let number, label = make()
    discard number
    discard label
}
"#,
    );
    let prepared = prepare_native_object(
        &managed_tuple,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare tuple with a direct managed Text leaf");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let managed_sum = compile_source(
        r#"record Label { value Text }

enum Message { Textual(Label) }

pub fn main() {
    discard Message.Textual(Label { value = "value" })
}
"#,
    );
    let prepared = prepare_native_object(
        &managed_sum,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare sum whose payload transitively contains managed Text");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let dead_text = compile_source(
        r#"pub fn main() {}

fn dead() Text { "unreachable" }
"#,
    );
    let prepared = prepare_native_object(
        &dead_text,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare artifact with unreachable unsupported code");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
}

#[test]
fn typed_timer_test_keeps_the_ordered_test_artifact_on_lcir() {
    let program = compile_sources(
        "",
        r"test fn scalar() {}

test async fn timer() {
    Task.sleep(1).await
}
",
    );
    for policy in [NativeRoutePolicy::Automatic, NativeRoutePolicy::LcirOnly] {
        let prepared = prepare_native_object(&program, EmitOptions::tests(), policy)
            .expect("prepare typed timer test artifact");
        assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    }
}

#[test]
fn sync_task_creation_uses_the_typed_hidden_executor_abi() {
    let directory = tempfile::tempdir().expect("create sync-task output directory");
    for (name, helper_count, program) in [
        ("direct", 1, sync_task_creation_program(false)),
        ("nested", 2, sync_task_creation_program(true)),
    ] {
        let ir = directory.path().join(format!("sync-task-{name}.ll"));
        let object = directory.path().join(format!("sync-task-{name}.o"));
        let mut options = EmitOptions::run("main");
        options.emit_ir = Some(ir.clone());
        let prepared = prepare_native_object(&program, options, NativeRoutePolicy::Automatic)
            .expect("prepare typed sync Task helper");
        assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
        emit_prepared_native_object(&prepared, &object).expect("emit typed LCIR artifact");
        assert!(object.is_file());
        let ir = std::fs::read_to_string(ir).expect("read selected LCIR IR");
        assert!(ir.contains("loom.lcir"), "{ir}");
        assert!(!ir.contains("%loom.Value"), "{ir}");
        assert_eq!(
            ir.lines()
                .filter(|line| {
                    line.starts_with("define internal ptr @loom.lcir.fn.")
                        && line.contains("(ptr %__loom_executor)")
                })
                .count(),
            helper_count,
            "{ir}"
        );

        let forced = prepare_native_object(
            &program,
            EmitOptions::run("main"),
            NativeRoutePolicy::LcirOnly,
        )
        .expect("forced LCIR must accept typed sync executor threading");
        assert_eq!(forced.route_kind(), NativeRouteKind::Lcir);
    }
}

#[test]
fn sync_executor_roots_never_fallback_around_unsupported_sites() {
    for (name, program) in [
        (
            "classifier",
            sync_executor_root_with_unsupported_site_program(),
        ),
        (
            "instance-closure",
            sync_executor_root_with_nonregular_instance_program(),
        ),
    ] {
        for policy in [NativeRoutePolicy::Automatic, NativeRoutePolicy::LcirOnly] {
            let error = prepare_native_object(&program, EmitOptions::run("main"), policy)
                .err()
                .unwrap_or_else(|| {
                    panic!("{name} executor-dependent synchronous root selected fallback")
                });
            assert_eq!(error.kind(), NativePreparationErrorKind::InvalidRoot);
            assert_eq!(error.code(), "NativePreparationRootCapability");
        }
    }
}

#[test]
fn empty_tests_are_a_complete_lcir_artifact() {
    let program = scalar_program();
    let prepared =
        prepare_native_object(&program, EmitOptions::tests(), NativeRoutePolicy::Automatic)
            .expect("prepare empty tests");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let directory = tempfile::tempdir().expect("create output directory");
    let object = directory.path().join("empty-tests.o");
    let emitted = emit_prepared_native_object(&prepared, &object).expect("emit empty tests");
    assert_eq!(emitted.functions, 0);
    assert!(object.is_file());
}

#[test]
fn invalid_roots_are_structured_and_never_fallback() {
    let program = scalar_program();
    for policy in [
        NativeRoutePolicy::Automatic,
        NativeRoutePolicy::LcirOnly,
        NativeRoutePolicy::CheckedMirOnly,
    ] {
        let error = prepare_native_object(&program, EmitOptions::run("missing"), policy)
            .err()
            .expect("missing root must fail preparation");
        assert_eq!(error.kind(), NativePreparationErrorKind::InvalidRoot);
        assert_eq!(error.code(), "NativePreparationUnknownEntry");
        assert!(error.message().contains("missing"));
    }
}

#[test]
fn lcir_only_accepts_a_complete_typed_artifact() {
    let program = scalar_program();
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::LcirOnly,
    )
    .expect("prepare required LCIR artifact");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
}

#[test]
fn lcir_only_preserves_a_deterministic_structured_support_report() {
    let program = unsupported_dynamic_program();
    let prepare = || {
        prepare_native_object(
            &program,
            EmitOptions::run("main"),
            NativeRoutePolicy::LcirOnly,
        )
        .err()
        .expect("unsupported LCIR must fail instead of selecting checked-MIR")
    };
    let first = prepare();
    let second = prepare();

    assert_eq!(first, second);
    assert_eq!(first.kind(), NativePreparationErrorKind::Unsupported);
    assert_eq!(first.code(), "NativePreparationUnsupportedLcir");
    let report = first
        .support_report()
        .expect("unsupported preparation carries its support report");
    assert!(!report.is_empty());
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::DynamicWitnessSet),
        "{report:#?}"
    );
    assert!(first.message().contains("DynamicWitnessSet"), "{first}");
    assert!(
        first.message().contains(report.items()[0].path()),
        "{first}"
    );
}

#[test]
fn checked_mir_only_never_attempts_the_lcir_route() {
    let program = scalar_program();
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::CheckedMirOnly,
    )
    .expect("prepare forced checked-MIR artifact");
    assert_eq!(prepared.route_kind(), NativeRouteKind::CheckedMir);
}

#[test]
fn selected_lcir_emitters_publish_typed_scalar_and_timer_surfaces() {
    let directory = tempfile::tempdir().expect("create output directory");
    let scalar = scalar_program();
    let scalar_ir = directory.path().join("scalar.ll");
    let mut scalar_options = EmitOptions::run("main");
    scalar_options.emit_ir = Some(scalar_ir.clone());
    let scalar_prepared =
        prepare_native_object(&scalar, scalar_options, NativeRoutePolicy::Automatic)
            .expect("prepare typed artifact");
    emit_prepared_native_object(&scalar_prepared, &directory.path().join("scalar.o"))
        .expect("emit typed LCIR");
    let scalar_ir = std::fs::read_to_string(scalar_ir).expect("read scalar IR");
    assert!(scalar_ir.contains("loom.lcir.fn"), "{scalar_ir}");
    assert!(!scalar_ir.contains("%loom.Value"), "{scalar_ir}");

    let timer = typed_timer_program();
    let timer_ir = directory.path().join("timer.ll");
    let mut timer_options = EmitOptions::run("main");
    timer_options.emit_ir = Some(timer_ir.clone());
    let timer_prepared = prepare_native_object(&timer, timer_options, NativeRoutePolicy::LcirOnly)
        .expect("prepare typed timer artifact");
    assert_eq!(timer_prepared.route_kind(), NativeRouteKind::Lcir);
    emit_prepared_native_object(&timer_prepared, &directory.path().join("timer.o"))
        .expect("emit typed timer LCIR");
    let timer_ir = std::fs::read_to_string(timer_ir).expect("read typed timer IR");
    for required in [
        "loom.lcir.fn",
        "loom_wait_now_ns",
        "loom_typed_timer_task_create_v1",
    ] {
        assert!(
            timer_ir.contains(required),
            "missing `{required}`:\n{timer_ir}"
        );
    }
    for forbidden in ["%loom.Value", "loom_task_from_wait_source"] {
        assert!(
            !timer_ir.contains(forbidden),
            "unexpected `{forbidden}`:\n{timer_ir}"
        );
    }
}

#[test]
fn lcir_emission_failure_does_not_change_the_prepared_route() {
    let program = scalar_program();
    let directory = tempfile::tempdir().expect("create output directory");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(directory.path().to_path_buf());
    let prepared = prepare_native_object(&program, options, NativeRoutePolicy::Automatic)
        .expect("prepare typed artifact");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);

    let object = directory.path().join("must-not-exist.o");
    let error = emit_prepared_native_object(&prepared, &object)
        .expect_err("invalid IR side path must fail LCIR emission");
    assert_eq!(error.code(), "LlvmIrWriteFailed");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    assert!(!object.exists());
}

#[test]
fn fingerprints_separate_routes_and_all_codegen_inputs() {
    let program = scalar_program();
    let fingerprint = |options, policy| {
        let prepared = prepare_native_object(&program, options, policy).expect("prepare object");
        prepared_native_object_fingerprint(&prepared).expect("fingerprint object")
    };
    let baseline = fingerprint(EmitOptions::run("main"), NativeRoutePolicy::Automatic);
    assert_eq!(
        baseline,
        fingerprint(EmitOptions::run("main"), NativeRoutePolicy::LcirOnly),
        "route policy must not perturb the selected LCIR object identity"
    );
    assert_ne!(
        baseline,
        fingerprint(EmitOptions::run("main"), NativeRoutePolicy::CheckedMirOnly),
        "LCIR and checked-MIR routes need disjoint identity domains"
    );
    assert_ne!(
        baseline,
        fingerprint(
            EmitOptions::run("main").with_optimization(OptimizationProfile::Release),
            NativeRoutePolicy::Automatic,
        )
    );
    assert_ne!(
        baseline,
        fingerprint(
            EmitOptions::run("main").with_debug_sources(vec![DebugSource::new(
                0,
                "main.loom",
                "Unit\n",
            )]),
            NativeRoutePolicy::Automatic,
        )
    );

    let host = target_identity(None, OptimizationProfile::Development).expect("host target");
    assert_ne!(
        baseline,
        fingerprint(
            EmitOptions::run("main").with_target_triple(Some(host.triple)),
            NativeRoutePolicy::Automatic,
        ),
        "an explicit portable target differs from the implicit tuned host"
    );

    let changed = compile_source(
        r"pub fn main() {
    discard 8
}
",
    );
    let changed = prepare_native_object(
        &changed,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("prepare changed object");
    assert_ne!(
        baseline,
        prepared_native_object_fingerprint(&changed).expect("fingerprint changed object")
    );
}

#[test]
fn tuple_semantics_participate_in_the_lcir_object_cache_identity() {
    let fingerprint = |source| {
        let program = compile_source(source);
        let prepared = prepare_native_object(
            &program,
            EmitOptions::run("main"),
            NativeRoutePolicy::Automatic,
        )
        .expect("prepare tuple artifact");
        assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
        prepared_native_object_fingerprint(&prepared).expect("fingerprint tuple artifact")
    };
    let boolean = fingerprint(
        r"fn consume(value (Int, Bool)) { discard value }

pub fn main() { consume((1, true)) }
",
    );
    let floating = fingerprint(
        r"fn consume(value (Int, Float)) { discard value }

pub fn main() { consume((1, 1.0)) }
",
    );

    assert_ne!(boolean, floating);
}

#[test]
fn closed_sum_semantics_participate_in_the_lcir_object_cache_identity() {
    let fingerprint = |source| {
        let program = compile_source(source);
        let prepared = prepare_native_object(
            &program,
            EmitOptions::run("main"),
            NativeRoutePolicy::Automatic,
        )
        .expect("prepare closed-sum artifact");
        assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
        prepared_native_object_fingerprint(&prepared).expect("fingerprint closed-sum artifact")
    };
    let boolean = fingerprint(
        r"enum Choice { Empty, Value(Bool) }

pub fn main() { discard Choice.Value(true) }
",
    );
    let floating = fingerprint(
        r"enum Choice { Empty, Value(Float) }

pub fn main() { discard Choice.Value(1.0) }
",
    );

    assert_ne!(boolean, floating);
}

#[test]
fn fingerprint_excludes_output_and_ir_side_artifact_paths() {
    let program = scalar_program();
    let fingerprint = |path| {
        let mut options = EmitOptions::run("main");
        options.emit_ir = Some(path);
        let prepared = prepare_native_object(&program, options, NativeRoutePolicy::Automatic)
            .expect("prepare object");
        prepared_native_object_fingerprint(&prepared).expect("fingerprint object")
    };
    assert_eq!(
        fingerprint("first.ll".into()),
        fingerprint("different/second.ll".into())
    );
}

#[test]
fn checked_mir_fingerprint_separates_run_and_test_harnesses_for_the_same_root() {
    let mut program = scalar_program().into_program();
    let root = program.exports["main"];
    program.tests.push(root);
    let program = program
        .into_checked()
        .expect("shared run and test root is valid checked MIR");
    let fingerprint = |options| {
        let prepared = prepare_native_object(&program, options, NativeRoutePolicy::CheckedMirOnly)
            .expect("prepare checked-MIR object");
        prepared_native_object_fingerprint(&prepared).expect("fingerprint checked-MIR object")
    };

    assert_ne!(
        fingerprint(EmitOptions::run("main")),
        fingerprint(EmitOptions::tests()),
        "the root graph is shared but the emitted main harness is not"
    );
}

#[test]
fn prepared_target_identity_matches_the_machine_policy() {
    let program = scalar_program();
    let host = target_identity(None, OptimizationProfile::Development).expect("host target");
    let options = EmitOptions::run("main")
        .with_target_triple(Some(host.triple.clone()))
        .with_optimization(OptimizationProfile::Release);
    let prepared = prepare_native_object(&program, options, NativeRoutePolicy::Automatic)
        .expect("prepare explicit release object");
    let expected = target_identity(Some(&host.triple), OptimizationProfile::Release)
        .expect("explicit release target");
    assert_eq!(prepared_native_target_identity(&prepared), &expected);
}

#[test]
fn thirty_two_bit_targets_are_allowed_only_for_complete_lcir() {
    let scalar = scalar_program();
    let options =
        EmitOptions::run("main").with_target_triple(Some("i686-unknown-linux-gnu".to_owned()));
    let prepared = prepare_native_object(&scalar, options, NativeRoutePolicy::Automatic)
        .expect("32-bit typed LCIR is representable");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    let directory = tempfile::tempdir().expect("create output directory");
    let object = directory.path().join("scalar-i686.o");
    emit_prepared_native_object(&prepared, &object).expect("emit 32-bit typed LCIR object");
    assert!(
        std::fs::read(object)
            .expect("read 32-bit object")
            .starts_with(b"\x7fELF")
    );

    let text = allocating_text_program();
    let options =
        EmitOptions::run("main").with_target_triple(Some("i686-unknown-linux-gnu".to_owned()));
    let error = prepare_native_object(&text, options, NativeRoutePolicy::LcirOnly)
        .err()
        .expect("LCIR-only must preserve unsupported coverage before checked-MIR ABI validation");
    assert_eq!(error.kind(), NativePreparationErrorKind::Unsupported);
    assert!(error.support_report().is_some());

    let text = allocating_text_program();
    let options =
        EmitOptions::run("main").with_target_triple(Some("i686-unknown-linux-gnu".to_owned()));
    let error = prepare_native_object(&text, options, NativeRoutePolicy::Automatic)
        .err()
        .expect("checked-MIR Value ABI must reject 32-bit targets");
    assert_eq!(error.kind(), NativePreparationErrorKind::Target);
    assert_eq!(error.code(), "UnsupportedNativePointerWidth");
}
