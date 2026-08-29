use loom_codegen_ir::{SourceRoots, analyze_source_reachability};
use loom_core::{FileId, Span};
use loom_driver::{AnalysisHost, format_source};
use loom_interpreter::{Interpreter, Value};
use loom_mir::{Builtin, CallTarget, CheckedProgram, ExprKind, Function, FunctionId, VariantId};
use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

fn function_named<'a>(program: &'a CheckedProgram, name: &str) -> &'a Function {
    program
        .functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing `{name}` in {program:#?}"))
}

fn direct_targets(function: &Function) -> BTreeSet<FunctionId> {
    function
        .exprs_preorder()
        .filter_map(|expression| {
            let ExprKind::Call { target, .. } = &expression.kind else {
                return None;
            };
            match target {
                CallTarget::Direct(target) | CallTarget::Inherent(target) => Some(*target),
                CallTarget::Builtin(_)
                | CallTarget::StaticConcept { .. }
                | CallTarget::Dynamic { .. } => None,
            }
        })
        .collect()
}

fn builtin_targets(function: &Function) -> BTreeSet<Builtin> {
    function
        .exprs_preorder()
        .filter_map(|expression| {
            let ExprKind::Call {
                target: CallTarget::Builtin(target),
                ..
            } = &expression.kind
            else {
                return None;
            };
            Some(*target)
        })
        .collect()
}

fn reachable_surface(
    program: &CheckedProgram,
    entry: &str,
) -> (BTreeSet<String>, BTreeSet<Builtin>) {
    let root = program
        .exports
        .get(entry)
        .copied()
        .unwrap_or_else(|| panic!("missing exported entry `{entry}`"));
    let reachable = analyze_source_reachability(program, &SourceRoots::one(root))
        .expect("close the production source call graph");
    let names = reachable
        .functions
        .iter()
        .map(|function| {
            program
                .function(*function)
                .unwrap_or_else(|| panic!("missing reachable function #{}", function.0))
                .name
                .clone()
        })
        .collect();
    (names, reachable.builtins)
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one vertical process boundary test keeps source authority, call-graph reachability, and interpreter behavior together"
)]
fn source_process_wrappers_are_direct_authoritative_and_reachable_only_on_demand() {
    let source = include_str!("../../../library/std/process/process.loom");
    let formatted = format_source(FileId(0), source);
    assert!(
        formatted.diagnostics.is_empty(),
        "{:#?}",
        formatted.diagnostics
    );
    assert_eq!(
        formatted.text, source,
        "std.process source must be canonical"
    );

    let project = tempfile::tempdir().expect("temporary source process project");
    std::fs::write(
        project.path().join("main.loom"),
        r"import std.process.arguments
import std.process.environment

pub fn idle() {}

pub fn argument_values() List[Text] {
    arguments()
}

pub fn environment_value(name Text) Option[Text] {
    environment(name)
}
",
    )
    .expect("write source process application");
    let snapshot = AnalysisHost::new(project.path())
        .expect("open source process project")
        .snapshot()
        .expect("analyze source process project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());

    let process_source = snapshot
        .sources()
        .documents()
        .iter()
        .find(|source| source.relative_path().ends_with("process/process.loom"))
        .expect("embedded std.process source");
    assert!(process_source.is_compiler_std());
    assert!(process_source.is_read_only());
    assert!(!process_source.is_navigable());
    assert!(
        process_source
            .package()
            .is_some_and(loom_core::PackageId::is_compiler_std)
    );

    let program = snapshot.executable().expect("lower source process MIR");
    let arguments = function_named(program, "std.process.arguments");
    let environment = function_named(program, "std.process.environment");
    let argument_values = function_named(program, "standalone.argument_values");
    let environment_value = function_named(program, "standalone.environment_value");

    assert_eq!(
        direct_targets(argument_values),
        BTreeSet::from([arguments.id])
    );
    assert_eq!(
        direct_targets(environment_value),
        BTreeSet::from([environment.id])
    );
    assert_eq!(
        builtin_targets(arguments),
        BTreeSet::from([Builtin::ProcessArguments])
    );
    assert_eq!(
        builtin_targets(environment),
        BTreeSet::from([Builtin::ProcessEnvironment])
    );
    assert!(builtin_targets(argument_values).is_empty());
    assert!(builtin_targets(environment_value).is_empty());

    let (idle_names, idle_builtins) = reachable_surface(program, "standalone.idle");
    assert_eq!(idle_names, BTreeSet::from(["standalone.idle".to_owned()]));
    assert!(idle_builtins.is_empty());

    let (environment_names, environment_builtins) =
        reachable_surface(program, "standalone.environment_value");
    assert!(environment_names.contains("std.process.environment"));
    assert!(!environment_names.contains("std.process.arguments"));
    assert!(environment_builtins.contains(&Builtin::ProcessEnvironment));
    assert!(!environment_builtins.contains(&Builtin::ProcessArguments));

    let mut interpreter = Interpreter::new(program).with_process_arguments(vec![
        "first".to_owned(),
        "界🙂".to_owned(),
        "last".to_owned(),
    ]);
    let argument_result = interpreter
        .invoke(argument_values.id, Vec::new(), Span::default())
        .expect("interpret source-backed process arguments");
    assert_eq!(
        argument_result,
        Value::List {
            elements: vec![
                Value::Text {
                    value: "first".to_owned(),
                },
                Value::Text {
                    value: "界🙂".to_owned(),
                },
                Value::Text {
                    value: "last".to_owned(),
                },
            ],
        }
    );

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let missing_name = format!(
        "LOOM_SOURCE_PROCESS_MISSING_{}_{}",
        std::process::id(),
        nonce
    );
    assert!(std::env::var_os(&missing_name).is_none());
    let missing = interpreter
        .invoke(
            environment_value.id,
            vec![Value::Text {
                value: missing_name,
            }],
            Span::default(),
        )
        .expect("interpret a missing environment value");
    let Value::Enum {
        variant, payload, ..
    } = missing
    else {
        panic!("missing environment result must be Option[Text]");
    };
    assert_eq!(variant, VariantId(0));
    assert!(payload.is_empty());
}
