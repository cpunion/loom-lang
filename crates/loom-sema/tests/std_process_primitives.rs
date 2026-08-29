use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{DefId, DefinitionKind, PackageSourceUnit, Program, lower_package_files};
use loom_sema::{Analysis, BuiltinValue, CallTarget, analyze};
use loom_syntax::parse_with_file;

const PROCESS_SOURCE: &str = r"
import std.process.__arguments
import std.process.__environment

pub fn arguments() List[Text] {
    __arguments()
}

pub fn environment(name Text) Option[Text] {
    __environment(name)
}
";

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

fn call_targets(program: &Program, analysis: &Analysis, definition: DefId) -> Vec<CallTarget> {
    let DefinitionKind::Function(function) = &program.definitions[definition].kind else {
        panic!("definition is not a function")
    };
    analysis
        .typed
        .body(function.body)
        .expect("checked function body")
        .calls
        .values()
        .map(|call| call.target.clone())
        .collect()
}

fn assert_no_process_primitive(
    program: &Program,
    analysis: &Analysis,
    package: &PackageId,
    module: &str,
    name: &str,
) {
    let definition = definition_named(program, package, module, name);
    assert!(
        call_targets(program, analysis, definition)
            .iter()
            .all(|target| !matches!(
                target,
                CallTarget::Builtin(
                    BuiltinValue::ProcessArguments | BuiltinValue::ProcessEnvironment
                )
            )),
        "unauthorized function received a process primitive: {package:?} {module}.{name}"
    );
}

fn assert_unknown_name(analysis: &Analysis, file: FileId) {
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "UnknownName" && diagnostic.primary.file == file),
        "missing authority failure for file {}: {:#?}",
        file.0,
        analysis.diagnostics
    );
}

#[test]
fn public_process_calls_resolve_to_source_functions_and_wrappers_own_builtins() {
    let std_file = FileId(0);
    let application_file = FileId(1);
    let process = parse_with_file(std_file, PROCESS_SOURCE);
    let application = parse_with_file(
        application_file,
        r"
import std.process.arguments
import std.process.environment

pub fn argument_values() List[Text] {
    arguments()
}

pub fn environment_value(name Text) Option[Text] {
    environment(name)
}
",
    );
    assert!(process.diagnostics().is_empty());
    assert!(application.diagnostics().is_empty());

    let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let application_package = PackageId::new("application", "0");
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: std_file,
            package: std_package.clone(),
            module: ModuleName::new("std.process"),
            syntax: process.ast(),
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
    lowered.program.register_package(
        application_package.clone(),
        [(Name::new("std"), std_package.clone())],
        true,
    );

    let analysis = analyze(&lowered.program);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    let arguments = definition_named(&lowered.program, &std_package, "std.process", "arguments");
    let environment =
        definition_named(&lowered.program, &std_package, "std.process", "environment");
    let argument_values = definition_named(
        &lowered.program,
        &application_package,
        "application",
        "argument_values",
    );
    let environment_value = definition_named(
        &lowered.program,
        &application_package,
        "application",
        "environment_value",
    );

    assert_eq!(
        call_targets(&lowered.program, &analysis, arguments),
        vec![CallTarget::Builtin(BuiltinValue::ProcessArguments)]
    );
    assert_eq!(
        call_targets(&lowered.program, &analysis, environment),
        vec![CallTarget::Builtin(BuiltinValue::ProcessEnvironment)]
    );
    assert_eq!(
        call_targets(&lowered.program, &analysis, argument_values),
        vec![CallTarget::Function(arguments)]
    );
    assert_eq!(
        call_targets(&lowered.program, &analysis, environment_value),
        vec![CallTarget::Function(environment)]
    );
}

#[test]
fn private_process_calls_reject_application_wrong_owner_and_wrong_package() {
    let process_file = FileId(0);
    let wrong_owner_file = FileId(1);
    let wrong_package_file = FileId(2);
    let application_file = FileId(3);
    let process = parse_with_file(process_file, PROCESS_SOURCE);
    let wrong_owner = parse_with_file(
        wrong_owner_file,
        r"
import std.process.__arguments

pub fn wrong_owner() List[Text] {
    __arguments()
}
",
    );
    let wrong_package = parse_with_file(
        wrong_package_file,
        r"
import std.process.__arguments

pub fn wrong_package() List[Text] {
    __arguments()
}
",
    );
    let application = parse_with_file(
        application_file,
        r"
import std.process.__arguments

pub fn application_private_import() List[Text] {
    __arguments()
}
",
    );
    for parsed in [&process, &wrong_owner, &wrong_package, &application] {
        assert!(
            parsed.diagnostics().is_empty(),
            "{:#?}",
            parsed.diagnostics()
        );
    }

    let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let hostile_package = PackageId::new("hostile-std", "0");
    let application_package = PackageId::new("application", "0");
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: process_file,
            package: std_package.clone(),
            module: ModuleName::new("std.process"),
            syntax: process.ast(),
        },
        PackageSourceUnit {
            file: wrong_owner_file,
            package: std_package.clone(),
            module: ModuleName::new("std.other"),
            syntax: wrong_owner.ast(),
        },
        PackageSourceUnit {
            file: wrong_package_file,
            package: hostile_package.clone(),
            module: ModuleName::new("std.process"),
            syntax: wrong_package.ast(),
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
        .register_package(hostile_package.clone(), [], false);
    lowered.program.register_package(
        application_package.clone(),
        [(Name::new("std"), std_package.clone())],
        true,
    );

    let analysis = analyze(&lowered.program);
    for file in [wrong_owner_file, wrong_package_file, application_file] {
        assert_unknown_name(&analysis, file);
    }

    for (package, module, name) in [
        (&std_package, "std.other", "wrong_owner"),
        (&hostile_package, "std.process", "wrong_package"),
        (
            &application_package,
            "application",
            "application_private_import",
        ),
    ] {
        assert_no_process_primitive(&lowered.program, &analysis, package, module, name);
    }
}
