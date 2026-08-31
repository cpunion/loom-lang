#![allow(clippy::default_trait_access)]

use loom_codegen_ir::INSTANCE_KEY_STRUCTURE_BUDGET;
use loom_codegen_llvm::{
    DebugSource, EmitOptions, NativePreparationErrorKind, OptimizationProfile,
    emit_prepared_native_object, prepare_native_object, prepared_native_object_fingerprint,
    prepared_native_target_identity, target_identity,
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

fn projected_product_program() -> CheckedProgram {
    compile_source(
        r"record Inner { left Int, right Int }
record Outer { inner Inner, enabled Bool }

impl Inner {
    method add(mut self, amount Int) Int {
        self.right = self.right + amount
        self.right
    }
}

fn projected() Int {
    var outer = Outer {
        inner = Inner { left = 1, right = 2 },
        enabled = true
    }
    discard outer.inner.add(3)
    outer.inner.right
}

pub fn main() {
    discard projected()
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

fn missing_dynamic_witness_program() -> CheckedProgram {
    compile_source(
        r"dyn concept Truth { method truth(self) Bool }

pub fn main() {
    discard missing().truth()
}

fn missing() dyn Truth { missing() }
",
    )
}

fn nonregular_generic_program() -> CheckedProgram {
    compile_source(
        r"fn spiral[T](value T) {
    spiral((value, value))
}

pub fn main() {
    spiral(0)
}
",
    )
}

fn oversized_generic_instance_program() -> CheckedProgram {
    let values = std::iter::repeat_n("value", INSTANCE_KEY_STRUCTURE_BUDGET)
        .collect::<Vec<_>>()
        .join(", ");
    compile_source(&format!(
        "fn expand[T](value T) {{\n    expand(({values}))\n}}\n\npub fn main() {{\n    expand(1)\n}}\n"
    ))
}

#[test]
fn preparation_is_atomic_over_the_reachable_typed_artifact() {
    let managed_tuple = compile_source(
        r#"fn make() (Int, Text) { (1, "value") }

pub fn main() {
    let number, label = make()
    discard number
    discard label
}
"#,
    );
    let managed_sum = compile_source(
        r#"record Label { value Text }

enum Message { Textual(Label) }

pub fn main() {
    discard Message.Textual(Label { value = "value" })
}
"#,
    );
    let dead_text = compile_source(
        r#"pub fn main() {}

fn dead() Text { "unreachable" }
"#,
    );
    for program in [
        scalar_program(),
        allocating_text_program(),
        text_get_program(),
        managed_tuple,
        managed_sum,
        dead_text,
    ] {
        prepare_native_object(&program, EmitOptions::run("main"))
            .expect("prepare complete typed artifact");
    }
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
    prepare_native_object(&program, EmitOptions::tests())
        .expect("prepare typed timer test artifact");
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
        let prepared =
            prepare_native_object(&program, options).expect("prepare typed sync Task helper");

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
    }
}

#[test]
fn sync_executor_roots_reject_unsupported_sites_atomically() {
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
        let error = prepare_native_object(&program, EmitOptions::run("main"))
            .err()
            .unwrap_or_else(|| panic!("{name} executor-dependent synchronous root was accepted"));
        assert_eq!(error.kind(), NativePreparationErrorKind::InvalidRoot);
        assert_eq!(error.code(), "NativePreparationRootCapability");
    }
}

#[test]
fn empty_tests_are_a_complete_lcir_artifact() {
    let program = scalar_program();
    let prepared =
        prepare_native_object(&program, EmitOptions::tests()).expect("prepare empty tests");

    let directory = tempfile::tempdir().expect("create output directory");
    let object = directory.path().join("empty-tests.o");
    let emitted = emit_prepared_native_object(&prepared, &object).expect("emit empty tests");
    assert_eq!(emitted.functions, 0);
    assert!(object.is_file());
}

#[test]
fn invalid_roots_are_structured() {
    let program = scalar_program();
    let error = prepare_native_object(&program, EmitOptions::run("missing"))
        .err()
        .expect("missing root must fail preparation");
    assert_eq!(error.kind(), NativePreparationErrorKind::InvalidRoot);
    assert_eq!(error.code(), "NativePreparationUnknownEntry");
    assert!(error.message().contains("missing"));
}

#[test]
fn nonregular_generic_recursion_is_rejected_before_native_lowering() {
    let program = nonregular_generic_program();
    let prepare = || {
        prepare_native_object(&program, EmitOptions::run("main"))
            .err()
            .expect("non-regular generic recursion must fail before native preparation")
    };
    let first = prepare();
    let second = prepare();
    assert_eq!(first, second);
    assert_eq!(first.kind(), NativePreparationErrorKind::InvalidProgram);
    assert_eq!(first.code(), "NonRegularGenericRecursion");
    assert!(first.support_report().is_none());
    assert!(first.message().contains(".body.tail.instance"), "{first}");
}

#[test]
fn generic_instance_budget_is_a_native_resource_error() {
    let program = oversized_generic_instance_program();
    let error = prepare_native_object(&program, EmitOptions::run("main"))
        .err()
        .expect("generic instance exhaustion must fail before native preparation");
    assert_eq!(error.kind(), NativePreparationErrorKind::Resource);
    assert_eq!(error.code(), "NativePreparationProgramTooLarge");
    assert!(error.support_report().is_none());
    assert!(error.message().contains(".body.tail"), "{error}");
}

#[test]
fn selected_lcir_emitters_publish_typed_scalar_and_timer_surfaces() {
    let directory = tempfile::tempdir().expect("create output directory");
    let scalar = scalar_program();
    let scalar_ir = directory.path().join("scalar.ll");
    let mut scalar_options = EmitOptions::run("main");
    scalar_options.emit_ir = Some(scalar_ir.clone());
    let scalar_prepared =
        prepare_native_object(&scalar, scalar_options).expect("prepare typed artifact");
    emit_prepared_native_object(&scalar_prepared, &directory.path().join("scalar.o"))
        .expect("emit typed LCIR");
    let scalar_ir = std::fs::read_to_string(scalar_ir).expect("read scalar IR");
    assert!(scalar_ir.contains("loom.lcir.fn"), "{scalar_ir}");
    assert!(!scalar_ir.contains("%loom.Value"), "{scalar_ir}");

    let timer = typed_timer_program();
    let timer_ir = directory.path().join("timer.ll");
    let mut timer_options = EmitOptions::run("main");
    timer_options.emit_ir = Some(timer_ir.clone());
    let timer_prepared =
        prepare_native_object(&timer, timer_options).expect("prepare typed timer artifact");

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
fn emission_failure_preserves_the_prepared_artifact() {
    let program = scalar_program();
    let directory = tempfile::tempdir().expect("create output directory");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(directory.path().to_path_buf());
    let prepared = prepare_native_object(&program, options).expect("prepare typed artifact");

    let object = directory.path().join("must-not-exist.o");
    let error = emit_prepared_native_object(&prepared, &object)
        .expect_err("invalid IR side path must fail LCIR emission");
    assert_eq!(error.code(), "LlvmIrWriteFailed");

    assert!(!object.exists());
}

#[test]
fn fingerprints_include_all_codegen_inputs() {
    let program = scalar_program();
    let fingerprint = |options| {
        let prepared = prepare_native_object(&program, options).expect("prepare object");
        prepared_native_object_fingerprint(&prepared).expect("fingerprint object")
    };
    let baseline = fingerprint(EmitOptions::run("main"));
    assert_ne!(
        baseline,
        fingerprint(EmitOptions::run("main").with_optimization(OptimizationProfile::Release),)
    );
    assert_ne!(
        baseline,
        fingerprint(
            EmitOptions::run("main").with_debug_sources(vec![DebugSource::new(
                0,
                "main.loom",
                "Unit\n",
            )]),
        )
    );

    let host = target_identity(None, OptimizationProfile::Development).expect("host target");
    assert_ne!(
        baseline,
        fingerprint(EmitOptions::run("main").with_target_triple(Some(host.triple)),),
        "an explicit portable target differs from the implicit tuned host"
    );

    let changed = compile_source(
        r"pub fn main() {
    discard 8
}
",
    );
    let changed =
        prepare_native_object(&changed, EmitOptions::run("main")).expect("prepare changed object");
    assert_ne!(
        baseline,
        prepared_native_object_fingerprint(&changed).expect("fingerprint changed object")
    );
}

#[test]
fn tuple_semantics_participate_in_the_lcir_object_cache_identity() {
    let fingerprint = |source| {
        let program = compile_source(source);
        let prepared = prepare_native_object(&program, EmitOptions::run("main"))
            .expect("prepare tuple artifact");

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
        let prepared = prepare_native_object(&program, EmitOptions::run("main"))
            .expect("prepare closed-sum artifact");

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
        let prepared = prepare_native_object(&program, options).expect("prepare object");
        prepared_native_object_fingerprint(&prepared).expect("fingerprint object")
    };
    assert_eq!(
        fingerprint("first.ll".into()),
        fingerprint("different/second.ll".into())
    );
}

#[test]
fn fingerprint_separates_run_and_test_harnesses_for_the_same_root() {
    let mut program = scalar_program().into_program();
    let root = program.exports["main"];
    program.tests.push(root);
    let program = program
        .into_checked()
        .expect("shared run and test root is valid checked MIR");
    let fingerprint = |options| {
        let prepared = prepare_native_object(&program, options).expect("prepare typed object");
        prepared_native_object_fingerprint(&prepared).expect("fingerprint typed object")
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
    let prepared =
        prepare_native_object(&program, options).expect("prepare explicit release object");
    let expected = target_identity(Some(&host.triple), OptimizationProfile::Release)
        .expect("explicit release target");
    assert_eq!(prepared_native_target_identity(&prepared), &expected);
}

#[test]
fn missing_dynamic_concept_witness_is_an_invalid_program_error() {
    let program = missing_dynamic_witness_program();
    let error = prepare_native_object(&program, EmitOptions::run("main"))
        .err()
        .expect("a missing dynamic concept witness must be rejected");
    assert_eq!(error.kind(), NativePreparationErrorKind::InvalidProgram);
    assert_eq!(error.code(), "MissingDynamicConceptWitness");
    assert!(error.support_report().is_none());
    assert!(
        error.message().contains("closed concept witness"),
        "{error}"
    );
}

#[test]
fn thirty_two_bit_targets_are_allowed_only_for_complete_lcir() {
    let directory = tempfile::tempdir().expect("create output directory");
    let object = directory.path().join("projected-product-i686.o");
    let ir_path = directory.path().join("projected-product-i686.ll");
    let mut options =
        EmitOptions::run("main").with_target_triple(Some("i686-unknown-linux-gnu".to_owned()));
    options.emit_ir = Some(ir_path.clone());
    let projected = projected_product_program();
    let prepared = prepare_native_object(&projected, options)
        .expect("32-bit nested product projection is representable");
    emit_prepared_native_object(&prepared, &object)
        .expect("emit 32-bit nested product projection object");
    assert!(
        std::fs::read(object)
            .expect("read 32-bit object")
            .starts_with(b"\x7fELF")
    );
    let ir = std::fs::read_to_string(ir_path).expect("read 32-bit projected-product LLVM IR");
    assert!(
        ir.contains("target triple = \"i686-unknown-linux-gnu\""),
        "{ir}"
    );
    assert!(
        ir.matches("insertvalue").count() >= 2,
        "nested projected writeback must rebuild both product levels:\n{ir}"
    );

    let text = allocating_text_program();
    let options =
        EmitOptions::run("main").with_target_triple(Some("i686-unknown-linux-gnu".to_owned()));
    let error = prepare_native_object(&text, options)
        .err()
        .expect("32-bit Text must fail at the typed representation boundary");
    assert_eq!(error.kind(), NativePreparationErrorKind::Unsupported);
    assert!(error.support_report().is_some());
}
