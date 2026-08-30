use std::path::Path;

use loom_codegen_llvm::{
    EmitOptions, NativeRouteKind, NativeRoutePolicy, emit_native_object,
    emit_prepared_native_object, native_object_fingerprint, prepare_native_object,
};
use loom_runtime_abi::TYPED_LOG_WRITE_SYMBOL;

mod support;

#[test]
fn logging_is_typed_lcir_only_and_checked_mir_cannot_bypass_it() {
    let project = source_program(
        r#"import std.log.info

pub fn main() {
    info("live")
}
"#,
    );
    let snapshot = support::analysis_host(project.path())
        .expect("load logging project")
        .snapshot()
        .expect("analyze logging project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower logging MIR");
    let options = EmitOptions::run("main");

    assert_eq!(
        native_object_fingerprint(program, &options)
            .expect_err("checked-MIR fingerprint must reject logging")
            .code(),
        "NativeLogRequiresLcir"
    );
    assert_eq!(
        emit_native_object(program, &project.path().join("checked.o"), &options)
            .expect_err("checked-MIR object emission must reject logging")
            .code(),
        "NativeLogRequiresLcir"
    );
    assert_eq!(
        support::emit_native(program, &project.path().join("checked"), &options)
            .expect_err("checked-MIR executable emission must reject logging")
            .code(),
        "NativeLogRequiresLcir"
    );

    let checked_only =
        prepare_native_object(program, options.clone(), NativeRoutePolicy::CheckedMirOnly);
    let Err(error) = checked_only else {
        panic!("checked-MIR route must reject logging");
    };
    assert_eq!(error.code(), "NativePreparationLogRequiresLcir");
    assert!(error.support_report().is_none());

    let ir_path = project.path().join("logging.ll");
    let object = project.path().join("logging.o");
    let mut typed_options = options;
    typed_options.emit_ir = Some(ir_path.clone());
    let prepared = prepare_native_object(program, typed_options, NativeRoutePolicy::Automatic)
        .expect("prepare typed logging LCIR");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    emit_prepared_native_object(&prepared, &object).expect("emit typed logging object");
    let ir = std::fs::read_to_string(ir_path).expect("read typed logging LLVM IR");
    assert!(ir.contains(TYPED_LOG_WRITE_SYMBOL), "{ir}");
    assert!(!ir.contains("@loom_runtime_log("), "{ir}");
    assert!(!ir.contains("%loom.Value"), "{ir}");

    assert_unsupported_logging_never_falls_back(project.path());
    assert_dead_logging_does_not_reject(project.path());
}

fn assert_unsupported_logging_never_falls_back(directory: &Path) {
    let project = source_program_in(
        directory,
        r#"import std.log.info

async fn child(value Int) Int { value }

pub async fn main() {
    info("live")
    let pending = Task.any(child(1), child(2))
    discard pending.await
}
"#,
    );
    let snapshot = support::analysis_host(project.path())
        .expect("load unsupported logging project")
        .snapshot()
        .expect("analyze unsupported logging project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot
        .executable()
        .expect("lower unsupported logging MIR");
    let prepared = prepare_native_object(
        program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    );
    let Err(error) = prepared else {
        panic!("unsupported logging must not select checked-MIR fallback");
    };
    assert_eq!(error.code(), "NativePreparationLogRequiresLcir");
    assert!(error.support_report().is_some());
}

fn assert_dead_logging_does_not_reject(directory: &Path) {
    let project = source_program_in(
        directory,
        r#"import std.log.info

fn unused() {
    info("dead")
}

pub fn main() {}
"#,
    );
    let snapshot = support::analysis_host(project.path())
        .expect("load dead-logging project")
        .snapshot()
        .expect("analyze dead-logging project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower dead-logging MIR");
    let options = EmitOptions::run("main");
    native_object_fingerprint(program, &options)
        .expect("unreachable logging must not reject checked-MIR identity");
    let prepared = prepare_native_object(program, options, NativeRoutePolicy::CheckedMirOnly)
        .expect("unreachable logging must not reject checked-MIR preparation");
    assert_eq!(prepared.route_kind(), NativeRouteKind::CheckedMir);
}

fn source_program(source: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("create logging project");
    std::fs::write(project.path().join("main.loom"), source).expect("write logging source");
    project
}

fn source_program_in(directory: &Path, source: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir_in(directory).expect("create nested logging project");
    std::fs::write(project.path().join("main.loom"), source).expect("write logging source");
    project
}
