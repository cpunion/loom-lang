use std::path::Path;

use loom_codegen_llvm::{
    EmitOptions, NativeRouteKind, NativeRoutePolicy, emit_native_object,
    emit_prepared_native_object, native_object_fingerprint, prepare_native_object,
};
use loom_runtime_abi::{
    TYPED_IO_CANCEL_SYMBOL, TYPED_IO_POLL_SYMBOL, TYPED_IO_TASK_CREATE_SYMBOL,
    TYPED_RESOURCE_CLOSE_SYMBOL,
};

mod support;

#[test]
fn file_and_socket_io_are_typed_lcir_only() {
    let project = source_program(
        r#"import std.file.try_open_read

pub async fn main() {
    match try_open_read("missing-io-abi-file").await {
        Ok(file) => {
            scoped file = file
        }
        Err(_) => {}
    }
}
"#,
    );
    let snapshot = support::analysis_host(project.path())
        .expect("load I/O ABI project")
        .snapshot()
        .expect("analyze I/O ABI project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower I/O ABI MIR");
    let options = EmitOptions::run("main");

    assert_eq!(
        native_object_fingerprint(program, &options)
            .expect_err("checked-MIR fingerprint must reject I/O")
            .code(),
        "NativeIoRequiresLcir"
    );
    assert_eq!(
        emit_native_object(program, &project.path().join("checked.o"), &options)
            .expect_err("checked-MIR object emission must reject I/O")
            .code(),
        "NativeIoRequiresLcir"
    );
    assert_eq!(
        support::emit_native(program, &project.path().join("checked"), &options)
            .expect_err("checked-MIR executable emission must reject I/O")
            .code(),
        "NativeIoRequiresLcir"
    );

    let checked_only =
        prepare_native_object(program, options.clone(), NativeRoutePolicy::CheckedMirOnly);
    let Err(error) = checked_only else {
        panic!("checked-MIR route must reject I/O");
    };
    assert_eq!(error.code(), "NativePreparationIoRequiresLcir");
    assert!(error.support_report().is_none());

    let ir_path = project.path().join("typed-io.ll");
    let object = project.path().join("typed-io.o");
    let mut typed_options = options;
    typed_options.emit_ir = Some(ir_path.clone());
    let prepared = prepare_native_object(program, typed_options, NativeRoutePolicy::Automatic)
        .expect("prepare typed I/O LCIR");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
    emit_prepared_native_object(&prepared, &object).expect("emit typed I/O object");
    let ir = std::fs::read_to_string(ir_path).expect("read typed I/O LLVM IR");
    for symbol in [
        TYPED_IO_TASK_CREATE_SYMBOL,
        TYPED_IO_POLL_SYMBOL,
        TYPED_IO_CANCEL_SYMBOL,
        TYPED_RESOURCE_CLOSE_SYMBOL,
    ] {
        assert!(ir.contains(symbol), "missing `{symbol}`:\n{ir}");
    }
    for removed in [
        "loom_file_open_read",
        "loom_file_create",
        "loom_file_try_open_read",
        "loom_file_try_create",
        "loom_file_read_text",
        "loom_file_try_read_text",
        "loom_file_write_text",
        "loom_file_try_write_text",
        "loom_socket_connect",
        "loom_socket_try_connect",
        "loom_socket_read_text",
        "loom_socket_try_read_text",
        "loom_socket_write_text",
        "loom_socket_try_write_text",
        "loom_io_close",
    ] {
        assert!(
            !ir.contains(&format!("@{removed}(")),
            "obsolete `{removed}` leaked into:\n{ir}"
        );
    }
    assert!(
        !ir.contains("%loom.Value"),
        "universal Value leaked into:\n{ir}"
    );

    assert_task_join_io_stays_on_lcir(project.path());
    assert_dead_io_does_not_reject(project.path());
}

fn assert_task_join_io_stays_on_lcir(directory: &Path) {
    let project = source_program_in(
        directory,
        r#"import std.file.try_open_read

async fn child(value Int) Int { value }

pub async fn main() {
    match try_open_read("missing-unsupported-io-file").await {
        Ok(file) => {
            scoped file = file
        }
        Err(_) => {}
    }
    let pending = Task.any(child(1), child(2))
    discard pending.await
}
"#,
    );
    let snapshot = support::analysis_host(project.path())
        .expect("load unsupported I/O project")
        .snapshot()
        .expect("analyze unsupported I/O project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower unsupported I/O MIR");
    let prepared = prepare_native_object(
        program,
        EmitOptions::run("main"),
        NativeRoutePolicy::Automatic,
    )
    .expect("Task join and I/O must share the typed LCIR route");
    assert_eq!(prepared.route_kind(), NativeRouteKind::Lcir);
}

fn assert_dead_io_does_not_reject(directory: &Path) {
    let project = source_program_in(
        directory,
        r#"import std.file.try_open_read

async fn unused() {
    match try_open_read("missing-dead-io-file").await {
        Ok(file) => {
            scoped file = file
        }
        Err(_) => {}
    }
}

pub fn main() {}
"#,
    );
    let snapshot = support::analysis_host(project.path())
        .expect("load dead-I/O project")
        .snapshot()
        .expect("analyze dead-I/O project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower dead-I/O MIR");
    let options = EmitOptions::run("main");
    native_object_fingerprint(program, &options)
        .expect("unreachable I/O must not reject checked-MIR identity");
    let prepared = prepare_native_object(program, options, NativeRoutePolicy::CheckedMirOnly)
        .expect("unreachable I/O must not reject checked-MIR preparation");
    assert_eq!(prepared.route_kind(), NativeRouteKind::CheckedMir);
}

fn source_program(source: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("create I/O project");
    std::fs::write(project.path().join("main.loom"), source).expect("write I/O source");
    project
}

fn source_program_in(directory: &Path, source: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir_in(directory).expect("create nested I/O project");
    std::fs::write(project.path().join("main.loom"), source).expect("write I/O source");
    project
}
