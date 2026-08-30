use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{DefId, DefinitionKind, PackageSourceUnit, Program, lower_package_files};
use loom_sema::{Analysis, BuiltinValue, CallTarget, Resolution, analyze};
use loom_syntax::parse_with_file;

const LOG_SOURCE: &str = include_str!("../../../library/std/log/log.loom");

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

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one authority test keeps public source calls, variants, and the sole private primitive boundary together"
)]
fn public_log_calls_and_variants_use_ordinary_source_definitions() {
    let std_file = FileId(0);
    let application_file = FileId(1);
    let log = parse_with_file(std_file, LOG_SOURCE);
    let application = parse_with_file(
        application_file,
        r#"
import std.log.LogLevel
import std.log.debug
import std.log.error
import std.log.info
import std.log.warn
import std.log.write

pub fn emit(level LogLevel) {
    write(level, "event", TextMap[Text]())
    debug("debug")
    info("info")
    warn("warn")
    error("error")
}

pub fn inspect(level LogLevel) {
    match level {
        LogLevel.Debug => Unit
        LogLevel.Info => Unit
        LogLevel.Warn => Unit
        LogLevel.Error => Unit
    }
}
"#,
    );
    assert!(log.diagnostics().is_empty(), "{:#?}", log.diagnostics());
    assert!(
        application.diagnostics().is_empty(),
        "{:#?}",
        application.diagnostics()
    );

    let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let application_package = PackageId::new("application", "0");
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: std_file,
            package: std_package.clone(),
            module: ModuleName::new("std.log"),
            syntax: log.ast(),
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
    let log_level = definition_named(&lowered.program, &std_package, "std.log", "LogLevel");
    assert_eq!(analysis.canonical_std_items.log_level, Some(log_level));
    let DefinitionKind::Enum(enumeration) = &lowered.program.definitions[log_level].kind else {
        panic!("LogLevel must be an ordinary source enum")
    };
    let variants = enumeration.variants.clone();
    assert_eq!(
        variants
            .iter()
            .map(|variant| {
                lowered.program.definitions[*variant]
                    .name
                    .as_ref()
                    .expect("named LogLevel variant")
                    .as_str()
            })
            .collect::<Vec<_>>(),
        ["Debug", "Info", "Warn", "Error"]
    );

    let write = definition_named(&lowered.program, &std_package, "std.log", "write");
    let write_without_fields = definition_named(
        &lowered.program,
        &std_package,
        "std.log",
        "write_without_fields",
    );
    assert_eq!(
        call_targets(&lowered.program, &analysis, write),
        vec![CallTarget::Builtin(BuiltinValue::LogWrite)]
    );
    let targets = call_targets(&lowered.program, &analysis, write_without_fields);
    assert!(
        targets.contains(&CallTarget::Function(write)),
        "{targets:#?}"
    );
    assert!(
        targets
            .iter()
            .all(|target| *target != CallTarget::Builtin(BuiltinValue::LogWrite)),
        "the helper must call the source write wrapper: {targets:#?}"
    );
    for name in ["debug", "info", "warn", "error"] {
        let wrapper = definition_named(&lowered.program, &std_package, "std.log", name);
        let targets = call_targets(&lowered.program, &analysis, wrapper);
        assert!(
            targets.contains(&CallTarget::Function(write_without_fields)),
            "{name} must call the source helper: {targets:#?}"
        );
        assert!(
            targets
                .iter()
                .all(|target| *target != CallTarget::Builtin(BuiltinValue::LogWrite)),
            "{name} must not receive primitive authority: {targets:#?}"
        );
    }

    let emit = definition_named(
        &lowered.program,
        &application_package,
        "application",
        "emit",
    );
    let targets = call_targets(&lowered.program, &analysis, emit);
    assert!(
        targets.contains(&CallTarget::Function(write)),
        "{targets:#?}"
    );
    assert!(
        targets
            .iter()
            .all(|target| *target != CallTarget::Builtin(BuiltinValue::LogWrite)),
        "public callers must not receive primitive authority: {targets:#?}"
    );

    let inspect = definition_named(
        &lowered.program,
        &application_package,
        "application",
        "inspect",
    );
    let DefinitionKind::Function(inspect) = &lowered.program.definitions[inspect].kind else {
        panic!("inspect must be a function")
    };
    let resolutions = analysis
        .typed
        .body(inspect.body)
        .expect("checked inspect body")
        .pattern_resolutions
        .values()
        .collect::<Vec<_>>();
    for variant in variants {
        assert!(
            resolutions.contains(&&Resolution::Definition(variant)),
            "LogLevel pattern did not resolve to source variant {variant:?}: {resolutions:#?}"
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one hostile fixture compares application, wrong-owner, and wrong-package primitive imports"
)]
fn private_log_primitive_rejects_wrong_owner_package_and_application() {
    let log_file = FileId(0);
    let wrong_owner_file = FileId(1);
    let wrong_package_file = FileId(2);
    let application_file = FileId(3);
    let log = parse_with_file(log_file, LOG_SOURCE);
    let wrong_owner = parse_with_file(
        wrong_owner_file,
        r#"
import std.log.LogLevel
import std.log.__write

pub fn wrong_owner(level LogLevel) {
    __write(level, "wrong owner", TextMap[Text]())
}
"#,
    );
    let wrong_package = parse_with_file(
        wrong_package_file,
        r#"
import std.log.__write

pub enum LogLevel {
    Debug
    Info
    Warn
    Error
}

pub fn wrong_package(level LogLevel) {
    __write(level, "wrong package", TextMap[Text]())
}
"#,
    );
    let application = parse_with_file(
        application_file,
        r#"
import std.log.LogLevel
import std.log.__write

pub fn application_private_import(level LogLevel) {
    __write(level, "application", TextMap[Text]())
}
"#,
    );
    for parsed in [&log, &wrong_owner, &wrong_package, &application] {
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
            file: log_file,
            package: std_package.clone(),
            module: ModuleName::new("std.log"),
            syntax: log.ast(),
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
            module: ModuleName::new("std.log"),
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
    lowered.program.register_package(
        hostile_package.clone(),
        [(Name::new("std"), std_package.clone())],
        false,
    );
    lowered.program.register_package(
        application_package.clone(),
        [(Name::new("std"), std_package.clone())],
        true,
    );

    let analysis = analyze(&lowered.program);
    for file in [wrong_owner_file, wrong_package_file, application_file] {
        assert!(
            analysis.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "UnknownName" && diagnostic.primary.file == file
            }),
            "missing private-authority failure for file {}: {:#?}",
            file.0,
            analysis.diagnostics
        );
    }
    for (package, module, name) in [
        (&std_package, "std.other", "wrong_owner"),
        (&hostile_package, "std.log", "wrong_package"),
        (
            &application_package,
            "application",
            "application_private_import",
        ),
    ] {
        let definition = definition_named(&lowered.program, package, module, name);
        assert!(
            call_targets(&lowered.program, &analysis, definition)
                .iter()
                .all(|target| *target != CallTarget::Builtin(BuiltinValue::LogWrite)),
            "unauthorized function received LogWrite: {package:?} {module}.{name}"
        );
    }
}

#[test]
fn same_named_application_log_level_cannot_replace_the_canonical_source_type() {
    let log_file = FileId(0);
    let application_file = FileId(1);
    let log = parse_with_file(log_file, LOG_SOURCE);
    let application = parse_with_file(
        application_file,
        r#"
import std.log.write

pub enum LogLevel {
    Debug
    Info
    Warn
    Error
}

pub fn emit(level LogLevel) {
    write(level, "forged", TextMap[Text]())
}
"#,
    );
    assert!(log.diagnostics().is_empty(), "{:#?}", log.diagnostics());
    assert!(
        application.diagnostics().is_empty(),
        "{:#?}",
        application.diagnostics()
    );

    let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let application_package = PackageId::new("application", "0");
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: log_file,
            package: std_package.clone(),
            module: ModuleName::new("std.log"),
            syntax: log.ast(),
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
    let canonical = definition_named(&lowered.program, &std_package, "std.log", "LogLevel");
    let forged = definition_named(
        &lowered.program,
        &application_package,
        "application",
        "LogLevel",
    );
    assert_eq!(analysis.canonical_std_items.log_level, Some(canonical));
    assert_ne!(canonical, forged);
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "TypeMismatch" && diagnostic.primary.file == application_file
        }),
        "same-named application enum gained canonical authority: {:#?}",
        analysis.diagnostics
    );

    let emit = definition_named(
        &lowered.program,
        &application_package,
        "application",
        "emit",
    );
    assert!(
        call_targets(&lowered.program, &analysis, emit)
            .iter()
            .all(|target| *target != CallTarget::Builtin(BuiltinValue::LogWrite)),
        "public write call must not expose its private primitive"
    );
}
