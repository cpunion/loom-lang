use std::process::Command;

use loom_codegen_llvm::EmitOptions;
use loom_mir::{Builtin, CallTarget, ExprKind, Function, FunctionId};

mod support;
use support::emit_native;

#[test]
fn process_source_wrappers_own_the_private_runtime_primitives() {
    let source = r#"import std.process.arguments
import std.process.environment

pub fn main() {
    let values = arguments()
    let length = values.length()
    assert length == 2
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
    assert_eq!(builtin_call_count(main, Builtin::ProcessArguments), 0);
    assert_eq!(builtin_call_count(main, Builtin::ProcessEnvironment), 0);
    assert_eq!(builtin_call_count(arguments, Builtin::ProcessArguments), 1);
    assert_eq!(
        builtin_call_count(arguments, Builtin::ProcessEnvironment),
        0
    );
    assert_eq!(
        builtin_call_count(environment, Builtin::ProcessArguments),
        0
    );
    assert_eq!(
        builtin_call_count(environment, Builtin::ProcessEnvironment),
        1
    );

    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit process ABI executable");
    let output = Command::new(executable)
        .args(["alpha", "beta"])
        .env("LOOM_PROCESS_ABI_TEST", "present")
        .output()
        .expect("run process ABI executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
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
