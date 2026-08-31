use std::path::Path;
use std::process::Command;

use loom_codegen_llvm::{
    EmitOptions, emit_prepared_native_object, native_object_fingerprint, prepare_native_object,
};
use loom_mir::{Builtin, CallTarget, ExprKind, Function, FunctionId};
use loom_runtime_abi::{
    PROCESS_ARGUMENT_AT_TYPED_SYMBOL, PROCESS_ARGUMENT_COUNT_TYPED_SYMBOL,
    PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL, PROCESS_ENVIRONMENT_TYPED_SYMBOL,
};

mod support;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one vertical test pins source ownership, LLVM ABI, and native behavior"
)]
fn process_primitives_use_typed_lcir_and_run_natively() {
    let source = r#"import std.process.arguments
import std.process.environment

pub fn main() {
    let values = arguments()
    match values.get(0) {
        Some(value) => {
            assert value == "alpha"
            Unit
        }
        None => {
            assert false
            Unit
        }
    }
    match environment("LOOM_PROCESS_ABI_TEST") {
        Some(value) => {
            assert value == "present"
            Unit
        }
        None => {
            assert false
            Unit
        }
    }
    match values.get(1) {
        Some(value) => {
            assert value == "beta"
            Unit
        }
        None => {
            assert false
            Unit
        }
    }
    match values.get(2) {
        Some(value) => {
            assert value == "界🙂"
            Unit
        }
        None => {
            assert false
            Unit
        }
    }
}
"#;
    let project = tempfile::tempdir().expect("create process ABI project");
    std::fs::write(project.path().join("main.loom"), source).expect("write process ABI source");
    let snapshot = support::analysis_host(project.path())
        .expect("load process ABI project")
        .snapshot()
        .expect("analyze process ABI project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower process ABI MIR");
    let main = named_function(program.functions.as_slice(), "standalone.main");
    let arguments = named_function(program.functions.as_slice(), "std.process.arguments");
    let environment = named_function(program.functions.as_slice(), "std.process.environment");

    assert_eq!(direct_call_count(main, arguments.id), 1);
    assert_eq!(direct_call_count(main, environment.id), 1);
    for primitive in [
        Builtin::ProcessArgumentCount,
        Builtin::ProcessArgumentAt,
        Builtin::ProcessEnvironment,
    ] {
        assert_eq!(builtin_call_count(main, primitive), 0);
    }
    assert_eq!(
        builtin_call_count(arguments, Builtin::ProcessArgumentCount),
        1
    );
    assert_eq!(builtin_call_count(arguments, Builtin::ProcessArgumentAt), 1);
    assert_eq!(
        builtin_call_count(arguments, Builtin::ProcessEnvironment),
        0
    );
    assert_eq!(
        builtin_call_count(environment, Builtin::ProcessArgumentCount),
        0
    );
    assert_eq!(
        builtin_call_count(environment, Builtin::ProcessArgumentAt),
        0
    );
    assert_eq!(
        builtin_call_count(environment, Builtin::ProcessEnvironment),
        1
    );

    let options = EmitOptions::run("main");

    let ir_path = project.path().join("process.ll");
    let object = project.path().join("process.o");
    let executable = project.path().join("process");
    let mut options = options;
    options.emit_ir = Some(ir_path.clone());
    let prepared = prepare_native_object(program, options).expect("prepare typed process LCIR");

    emit_prepared_native_object(&prepared, &object).expect("emit typed process object");
    support::link_native_object(&object, &executable).expect("link typed process executable");

    let ir = std::fs::read_to_string(&ir_path).expect("read typed process LLVM IR");
    for symbol in [
        PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL,
        PROCESS_ARGUMENT_COUNT_TYPED_SYMBOL,
        PROCESS_ARGUMENT_AT_TYPED_SYMBOL,
        PROCESS_ENVIRONMENT_TYPED_SYMBOL,
    ] {
        assert!(ir.contains(symbol), "missing `{symbol}`:\n{ir}");
    }
    for removed in [
        "loom_runtime_set_arguments",
        "loom_runtime_process_arguments",
        "loom_runtime_process_environment",
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
    let main_ir = llvm_function_calling(&ir, PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL);
    assert!(main_ir.starts_with("define i32 @main("), "{main_ir}");
    assert!(
        main_ir
            .find(PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL)
            .expect("argument initialization call")
            < main_ir
                .find("loom_runtime_create_v1")
                .expect("runtime creation call"),
        "argument snapshot must precede runtime creation:\n{main_ir}"
    );
    let argument_at_ir = llvm_function_calling(&ir, PROCESS_ARGUMENT_AT_TYPED_SYMBOL);
    let argument_at_call = argument_at_ir
        .find(PROCESS_ARGUMENT_AT_TYPED_SYMBOL)
        .expect("typed process call");
    assert!(
        argument_at_ir[..argument_at_call].contains("store i64"),
        "managed-root state was not published before argument selection:\n{argument_at_ir}"
    );
    let count_ir = llvm_function_calling(&ir, PROCESS_ARGUMENT_COUNT_TYPED_SYMBOL);
    assert!(count_ir.contains("icmp sgt i64"), "{count_ir}");
    assert!(
        argument_at_ir.lines().any(|line| {
            line.contains(&format!(
                "call i32 @{PROCESS_ARGUMENT_AT_TYPED_SYMBOL}(i64 "
            )) && line.matches("ptr ").count() == 1
        }),
        "{argument_at_ir}"
    );
    let environment_ir = llvm_function_calling(&ir, PROCESS_ENVIRONMENT_TYPED_SYMBOL);
    assert!(
        environment_ir.lines().any(|line| {
            line.contains(&format!(
                "call i32 @{PROCESS_ENVIRONMENT_TYPED_SYMBOL}(ptr "
            )) && line.matches("ptr ").count() == 2
        }),
        "{environment_ir}"
    );
    assert!(environment_ir.contains("icmp eq i32"), "{environment_ir}");
    assert!(environment_ir.contains("llvm.trap"), "{environment_ir}");
    let environment_wrapper = llvm_defined_symbol(environment_ir);
    let environment_caller = llvm_function_calling(&ir, environment_wrapper);
    let environment_call = environment_caller
        .find(environment_wrapper)
        .expect("environment wrapper call");
    assert!(
        environment_caller[..environment_call].contains("store i64"),
        "caller did not publish its live List root before environment lookup:\n{environment_caller}"
    );

    let output = Command::new(executable)
        .args(["alpha", "beta", "界🙂"])
        .env("LOOM_PROCESS_ABI_TEST", "present")
        .output()
        .expect("run process ABI executable");
    assert!(
        output.status.success(),
        "status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");

    assert_environment_only_omits_argument_snapshot(project.path());
    assert_task_join_process_stays_on_lcir(project.path());
    assert_dead_process_does_not_reject(project.path());
}

fn assert_environment_only_omits_argument_snapshot(directory: &Path) {
    let source = r#"import std.process.environment

pub fn main() {
    match environment("LOOM_PROCESS_ENVIRONMENT_ONLY_MISSING") {
        Some(value) => {
            discard value
            assert false
            Unit
        }
        None => {
            Unit
        }
    }
}
"#;
    let project = tempfile::tempdir_in(directory).expect("create environment-only project");
    std::fs::write(project.path().join("main.loom"), source)
        .expect("write environment-only source");
    let snapshot = support::analysis_host(project.path())
        .expect("load environment-only project")
        .snapshot()
        .expect("analyze environment-only project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower environment-only MIR");
    let ir_path = project.path().join("environment.ll");
    let object = project.path().join("environment.o");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir_path.clone());
    let prepared = prepare_native_object(program, options).expect("prepare environment-only LCIR");

    emit_prepared_native_object(&prepared, &object).expect("emit environment-only object");
    let ir = std::fs::read_to_string(ir_path).expect("read environment-only IR");
    assert!(ir.contains(PROCESS_ENVIRONMENT_TYPED_SYMBOL), "{ir}");
    assert!(
        !ir.contains(PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL),
        "{ir}"
    );
}

fn assert_task_join_process_stays_on_lcir(directory: &Path) {
    let source = r"import std.process.arguments

async fn child(value Int) Int { value }

pub async fn main() {
    discard arguments()
    let pending = Task.any(child(1), child(2))
    discard pending.await
}
";
    let project = tempfile::tempdir_in(directory).expect("create unsupported process project");
    std::fs::write(project.path().join("main.loom"), source)
        .expect("write unsupported process source");
    let snapshot = support::analysis_host(project.path())
        .expect("load unsupported process project")
        .snapshot()
        .expect("analyze unsupported process project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot
        .executable()
        .expect("lower unsupported process MIR");
    prepare_native_object(program, EmitOptions::run("main"))
        .expect("Task join and process access must share one typed artifact");
}

fn assert_dead_process_does_not_reject(directory: &Path) {
    let source = r"import std.process.arguments

fn unused() {
    discard arguments()
}

pub fn main() {}
";
    let project = tempfile::tempdir_in(directory).expect("create dead-process project");
    std::fs::write(project.path().join("main.loom"), source).expect("write dead-process source");
    let snapshot = support::analysis_host(project.path())
        .expect("load dead-process project")
        .snapshot()
        .expect("analyze dead-process project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower dead-process MIR");
    let options = EmitOptions::run("main");
    native_object_fingerprint(program, &options)
        .expect("unreachable process primitives must not affect the typed object identity");
    prepare_native_object(program, options)
        .expect("unreachable process primitives must not affect typed native preparation");
}

fn llvm_function_calling<'ir>(ir: &'ir str, symbol: &str) -> &'ir str {
    let needle = format!("@{symbol}(");
    let call = ir
        .match_indices(&needle)
        .find_map(|(offset, _)| {
            let line = &ir[ir[..offset].rfind('\n').map_or(0, |line| line + 1)..offset];
            line.contains("call ").then_some(offset)
        })
        .unwrap_or_else(|| panic!("missing call `{symbol}`:\n{ir}"));
    let start = ir[..call]
        .rfind("\ndefine ")
        .map_or(0, |start| start.saturating_add(1));
    let end = ir[call..]
        .find("\n}")
        .map_or(ir.len(), |end| call.saturating_add(end).saturating_add(2));
    &ir[start..end]
}

fn llvm_defined_symbol(function: &str) -> &str {
    function
        .lines()
        .next()
        .and_then(|line| line.split_once('@'))
        .and_then(|(_, rest)| rest.split_once('('))
        .map_or_else(
            || panic!("missing defined function symbol:\n{function}"),
            |(symbol, _)| symbol,
        )
}

fn named_function<'program>(functions: &'program [Function], name: &str) -> &'program Function {
    functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing MIR function `{name}`"))
}

fn direct_call_count(function: &Function, callee: FunctionId) -> usize {
    function
        .exprs_preorder()
        .filter(|expression| {
            matches!(
                &expression.kind,
                ExprKind::Call {
                    target: CallTarget::Direct(target),
                    ..
                } if *target == callee
            )
        })
        .count()
}

fn builtin_call_count(function: &Function, builtin: Builtin) -> usize {
    function
        .exprs_preorder()
        .filter(|expression| {
            matches!(
                &expression.kind,
                ExprKind::Call {
                    target: CallTarget::Builtin(target),
                    ..
                } if *target == builtin
            )
        })
        .count()
}
