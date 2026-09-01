use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{DefId, DefinitionKind, PackageSourceUnit, Program, lower_package_files};
use loom_sema::{
    Analysis, BuiltinType, CallTarget, Coercion, ConstructionCheck, TaskIntrinsic, TyData, analyze,
};
use loom_syntax::parse_with_file;

const TIME_SOURCE: &str = include_str!("../../../library/std/time/time.loom");

fn definition_named(program: &Program, package: &PackageId, module: &str, name: &str) -> DefId {
    program
        .definitions
        .iter()
        .find_map(|(definition, item)| {
            let owner = &program.modules[item.module];
            (owner.package == *package
                && owner.name.as_str() == module
                && item
                    .name
                    .as_ref()
                    .is_some_and(|candidate| candidate.as_str() == name))
            .then_some(definition)
        })
        .unwrap_or_else(|| panic!("missing {package:?} {module}.{name}"))
}

fn analyze_with_time(
    library_source: &str,
    application_source: &str,
) -> (Program, Analysis, PackageId, PackageId, PackageId) {
    let time_file = FileId(0);
    let library_file = FileId(1);
    let application_file = FileId(2);
    let time = parse_with_file(time_file, TIME_SOURCE);
    let library = parse_with_file(library_file, library_source);
    let application = parse_with_file(application_file, application_source);
    assert!(time.diagnostics().is_empty(), "{:#?}", time.diagnostics());
    assert!(
        library.diagnostics().is_empty(),
        "{:#?}",
        library.diagnostics()
    );
    assert!(
        application.diagnostics().is_empty(),
        "{:#?}",
        application.diagnostics()
    );

    let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let library_package = PackageId::new("library", "0");
    let application_package = PackageId::new("application", "0");
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: time_file,
            package: std_package.clone(),
            module: ModuleName::new("std.time"),
            syntax: time.ast(),
        },
        PackageSourceUnit {
            file: library_file,
            package: library_package.clone(),
            module: ModuleName::new("library"),
            syntax: library.ast(),
        },
        PackageSourceUnit {
            file: application_file,
            package: application_package.clone(),
            module: ModuleName::new("application"),
            syntax: application.ast(),
        },
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    lowered
        .program
        .register_package(std_package.clone(), [], false);
    lowered
        .program
        .register_package(library_package.clone(), [], false);
    lowered.program.register_package(
        application_package.clone(),
        [
            (Name::new("std"), std_package.clone()),
            (Name::new("library"), library_package.clone()),
        ],
        true,
    );
    let analysis = analyze(&lowered.program);
    (
        lowered.program,
        analysis,
        std_package,
        library_package,
        application_package,
    )
}

fn body_calls(program: &Program, analysis: &Analysis, definition: DefId) -> Vec<CallTarget> {
    let body = match &program.definitions[definition].kind {
        DefinitionKind::Function(function) => function.body,
        DefinitionKind::Method(method) => method.body.expect("source method body"),
        _ => panic!("definition has no callable body"),
    };
    analysis
        .typed
        .body(body)
        .expect("checked callable body")
        .calls
        .values()
        .map(|call| call.target.clone())
        .collect()
}

#[test]
fn duration_type_constructor_method_and_sleep_use_source_identity() {
    let (program, analysis, std_package, _, application_package) = analyze_with_time(
        "",
        r"
import std.time.milliseconds

pub async fn wait() Int {
    let duration = milliseconds(1)
    discard Task.sleep(duration).await
    duration.as_milliseconds()
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );

    let duration = definition_named(&program, &std_package, "std.time", "Duration");
    let milliseconds = definition_named(&program, &std_package, "std.time", "milliseconds");
    let as_milliseconds = definition_named(&program, &std_package, "std.time", "as_milliseconds");
    let wait = definition_named(&program, &application_package, "application", "wait");
    let DefinitionKind::RefinedType(refined) = &program.definitions[duration].kind else {
        panic!("Duration must be an ordinary source constrained type")
    };
    assert_eq!(
        analysis
            .typed
            .resolved_type_refs
            .get(refined.base)
            .map(|ty| analysis.typed.types.data(*ty)),
        Some(&TyData::Builtin(BuiltinType::Int))
    );

    assert!(
        body_calls(&program, &analysis, milliseconds)
            .contains(&CallTarget::RefinedConstructor(duration)),
        "milliseconds must call the ordinary source constrained constructor"
    );
    let DefinitionKind::Function(milliseconds_source) = &program.definitions[milliseconds].kind
    else {
        panic!("milliseconds must be a source function")
    };
    let milliseconds_body = analysis
        .typed
        .body(milliseconds_source.body)
        .expect("checked milliseconds body");
    let (constructor, _) = milliseconds_body
        .calls
        .iter()
        .find(|(_, call)| call.target == CallTarget::RefinedConstructor(duration))
        .expect("milliseconds constrained constructor call");
    assert_eq!(
        milliseconds_body.construction_checks.get(constructor),
        Some(&ConstructionCheck::Precondition { index: 0 })
    );
    let method_calls = body_calls(&program, &analysis, wait);
    assert!(method_calls.contains(&CallTarget::Function(milliseconds)));
    assert!(method_calls.contains(&CallTarget::InherentMethod(as_milliseconds)));
    assert!(method_calls.contains(&CallTarget::TaskIntrinsic(TaskIntrinsic::Sleep)));

    let DefinitionKind::Function(wait_source) = &program.definitions[wait].kind else {
        panic!("wait must be a function")
    };
    let wait_body = analysis
        .typed
        .body(wait_source.body)
        .expect("checked wait body");
    let (sleep_argument, _) = wait_body
        .expression_coercions
        .iter()
        .find(|(_, coercion)| **coercion == Coercion::RefinedToBase { refined: duration })
        .expect("Task.sleep Duration argument must be unrefined");
    let normalized = wait_body
        .expression_types
        .get(sleep_argument)
        .expect("coerced sleep argument type");
    assert_eq!(
        analysis.typed.types.data(*normalized),
        &TyData::Builtin(BuiltinType::Int)
    );

    let DefinitionKind::Method(method_source) = &program.definitions[as_milliseconds].kind else {
        panic!("as_milliseconds must be a source method")
    };
    let method_body = analysis
        .typed
        .body(method_source.body.expect("source Duration method body"))
        .expect("checked Duration method body");
    assert!(
        method_body
            .expression_coercions
            .values()
            .any(|coercion| { *coercion == Coercion::RefinedToBase { refined: duration } })
    );
}

#[test]
fn application_refined_int_uses_the_general_task_sleep_coercion() {
    let (program, analysis, _, _, application_package) = analyze_with_time(
        "",
        r"
pub type Delay = Int where self >= 0

pub async fn wait() {
    discard Task.sleep(Delay(1)).await
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    let delay = definition_named(&program, &application_package, "application", "Delay");
    let wait = definition_named(&program, &application_package, "application", "wait");
    let DefinitionKind::Function(wait_source) = &program.definitions[wait].kind else {
        panic!("wait must be a function")
    };
    let wait_body = analysis
        .typed
        .body(wait_source.body)
        .expect("checked wait body");
    let (argument, _) = wait_body
        .expression_coercions
        .iter()
        .find(|(_, coercion)| **coercion == Coercion::RefinedToBase { refined: delay })
        .expect("Task.sleep must use the general refined-to-base coercion");
    assert_eq!(
        analysis.typed.types.data(
            *wait_body
                .expression_types
                .get(argument)
                .expect("argument type")
        ),
        &TyData::Builtin(BuiltinType::Int)
    );
}

#[test]
fn removed_private_milliseconds_import_is_unknown() {
    let (_, analysis, _, _, _) = analyze_with_time(
        "",
        r"
import std.time.__milliseconds

pub fn forbidden() {
    discard __milliseconds(1)
}
",
    );
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "UnknownName" && diagnostic.primary.file == FileId(2)
        }),
        "{:#?}",
        analysis.diagnostics
    );
}
