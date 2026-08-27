#![allow(clippy::default_trait_access)]

use loom_codegen_ir::UnsupportedFeature;
use loom_codegen_llvm::{
    DebugSource, EmitOptions, NativePreparationErrorKind, NativeRouteKind, NativeRoutePolicy,
    OptimizationProfile, emit_prepared_native_object, prepare_native_object,
    prepared_native_object_fingerprint, prepared_native_target_identity, target_identity,
};
use loom_driver::AnalysisHost;
use loom_mir::CheckedProgram;

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

fn scalar_program() -> CheckedProgram {
    compile_source(
        r"module prepared_scalar

fn choose(flag Bool) Int {
    if flag { 7 } else { 9 }
}

pub fn main() Unit {
    discard choose(true)
    Unit
}
",
    )
}

fn allocating_text_program() -> CheckedProgram {
    compile_source(
        r#"module prepared_text

pub fn main() Unit {
    discard "left".concat("right")
    Unit
}
"#,
    )
}

fn derived_text_program() -> CheckedProgram {
    compile_source(
        r#"module prepared_derived_text

pub fn main() Unit {
    discard "value".get(0)
    Unit
}
"#,
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

    let managed_tuple = compile_source(
        r#"module prepared_managed_tuple

fn make() (Int, Text) { (1, "legacy") }

pub fn main() Unit {
    let number, label = make()
    discard number
    discard label
    Unit
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
        r#"module prepared_managed_sum

record Label { value Text }

enum Message { Textual(Label) }

pub fn main() Unit {
    discard Message.Textual(Label { value = "legacy" })
    Unit
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
        r#"module prepared_dead_text

pub fn main() Unit { Unit }

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
fn one_unsupported_test_selects_legacy_for_the_ordered_test_artifact() {
    let program = compile_source(
        r#"module prepared_tests

test fn scalar() Unit { Unit }

test fn text() Unit {
    discard "value".get(0)
    Unit
}
"#,
    );
    let prepared =
        prepare_native_object(&program, EmitOptions::tests(), NativeRoutePolicy::Automatic)
            .expect("prepare complete test artifact");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Legacy);
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
        NativeRoutePolicy::LegacyOnly,
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
    let program = derived_text_program();
    let prepare = || {
        prepare_native_object(
            &program,
            EmitOptions::run("main"),
            NativeRoutePolicy::LcirOnly,
        )
        .err()
        .expect("unsupported LCIR must fail instead of selecting legacy")
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
            .any(|item| item.feature() == UnsupportedFeature::BuiltinCall),
        "{report:#?}"
    );
    assert!(first.message().contains("BuiltinCall"), "{first}");
    assert!(
        first.message().contains(report.items()[0].path()),
        "{first}"
    );
}

#[test]
fn legacy_only_never_attempts_the_lcir_route() {
    let program = scalar_program();
    let prepared = prepare_native_object(
        &program,
        EmitOptions::run("main"),
        NativeRoutePolicy::LegacyOnly,
    )
    .expect("prepare forced legacy artifact");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Legacy);
}

#[test]
fn selected_emitters_publish_disjoint_llvm_surfaces() {
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

    let text = derived_text_program();
    let text_ir = directory.path().join("text.ll");
    let mut text_options = EmitOptions::run("main");
    text_options.emit_ir = Some(text_ir.clone());
    let text_prepared = prepare_native_object(&text, text_options, NativeRoutePolicy::Automatic)
        .expect("prepare derived Text artifact");
    emit_prepared_native_object(&text_prepared, &directory.path().join("text.o"))
        .expect("emit legacy MIR");
    let text_ir = std::fs::read_to_string(text_ir).expect("read legacy IR");
    assert!(text_ir.contains("%loom.Value"), "{text_ir}");
    assert!(!text_ir.contains("loom.lcir.fn"), "{text_ir}");
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
        fingerprint(EmitOptions::run("main"), NativeRoutePolicy::LegacyOnly),
        "LCIR and legacy routes need disjoint identity domains"
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
        r"module prepared_scalar_changed

pub fn main() Unit {
    discard 8
    Unit
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
        r"module prepared_tuple_identity

fn consume(value (Int, Bool)) Unit { discard value }

pub fn main() Unit { consume((1, true)) }
",
    );
    let floating = fingerprint(
        r"module prepared_tuple_identity

fn consume(value (Int, Float)) Unit { discard value }

pub fn main() Unit { consume((1, 1.0)) }
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
        r"module prepared_sum_identity

enum Choice { Empty, Value(Bool) }

pub fn main() Unit { discard Choice.Value(true) }
",
    );
    let floating = fingerprint(
        r"module prepared_sum_identity

enum Choice { Empty, Value(Float) }

pub fn main() Unit { discard Choice.Value(1.0) }
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
fn legacy_fingerprint_separates_run_and_test_harnesses_for_the_same_root() {
    let mut program = scalar_program().into_program();
    let root = program.exports["main"];
    program.tests.push(root);
    let program = program
        .into_checked()
        .expect("shared run and test root is valid checked MIR");
    let fingerprint = |options| {
        let prepared = prepare_native_object(&program, options, NativeRoutePolicy::LegacyOnly)
            .expect("prepare legacy object");
        prepared_native_object_fingerprint(&prepared).expect("fingerprint legacy object")
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
        .expect("LCIR-only must preserve unsupported coverage before legacy ABI validation");
    assert_eq!(error.kind(), NativePreparationErrorKind::Unsupported);
    assert!(error.support_report().is_some());

    let text = allocating_text_program();
    let options =
        EmitOptions::run("main").with_target_triple(Some("i686-unknown-linux-gnu".to_owned()));
    let error = prepare_native_object(&text, options, NativeRoutePolicy::Automatic)
        .err()
        .expect("legacy Value ABI must reject 32-bit targets");
    assert_eq!(error.kind(), NativePreparationErrorKind::Target);
    assert_eq!(error.code(), "UnsupportedNativePointerWidth");
}
