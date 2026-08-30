use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{DefId, DefinitionKind, PackageSourceUnit, Program, lower_package_files};
use loom_sema::{Analysis, BuiltinValue, CallTarget, Resolution, Signature, TyData, analyze};
use loom_syntax::parse_with_file;

const IO_SOURCE: &str = include_str!("../../../library/std/io/io.loom");

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

fn analyze_with_io(application_source: &str) -> (Program, Analysis, PackageId, PackageId) {
    let std_file = FileId(0);
    let application_file = FileId(1);
    let io = parse_with_file(std_file, IO_SOURCE);
    let application = parse_with_file(application_file, application_source);
    assert!(io.diagnostics().is_empty(), "{:#?}", io.diagnostics());
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
            module: ModuleName::new("std.io"),
            syntax: io.ast(),
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
    (lowered.program, analysis, std_package, application_package)
}

fn callable_return_definition(program: &Program, analysis: &Analysis, definition: DefId) -> DefId {
    let Some(Signature::Callable(signature)) = analysis.typed.signatures.get(definition) else {
        panic!("definition is not callable")
    };
    let TyData::Nominal {
        definition,
        arguments,
    } = analysis.typed.types.data(signature.return_ty)
    else {
        panic!("callable return is not nominal")
    };
    assert!(arguments.is_empty(), "IoErrorKind must not be generic");
    assert!(matches!(
        program.definitions[*definition].kind,
        DefinitionKind::Enum(_)
    ));
    *definition
}

#[test]
fn public_output_resolves_to_source_and_only_the_wrapper_owns_the_primitive() {
    let (program, analysis, std_package, application_package) = analyze_with_io(
        r#"
import std.io.write

pub fn emit() {
    write("application")
}
"#,
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );

    let write = definition_named(&program, &std_package, "std.io", "write");
    let emit = definition_named(&program, &application_package, "application", "emit");
    assert_eq!(
        call_targets(&program, &analysis, write),
        vec![CallTarget::Builtin(BuiltinValue::StdoutWrite)]
    );
    assert_eq!(
        call_targets(&program, &analysis, emit),
        vec![CallTarget::Function(write)]
    );
}

#[test]
fn io_error_kind_prelude_variants_are_ordinary_source_definitions() {
    let (program, analysis, std_package, application_package) = analyze_with_io(
        r"
pub fn selected(flag Bool) IoErrorKind {
    if flag {
        IoErrorKind.NotFound
    } else {
        IoErrorKind.Other
    }
}

pub fn inspect(kind IoErrorKind) {
    match kind {
        IoErrorKind.NotFound => Unit
        std.io.IoErrorKind.PermissionDenied => Unit
        AlreadyExists => Unit
        InvalidInput => Unit
        ConnectionRefused => Unit
        ConnectionReset => Unit
        TimedOut => Unit
        UnexpectedEof => Unit
        Closed => Unit
        Other => Unit
    }
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );

    let kind = definition_named(&program, &std_package, "std.io", "IoErrorKind");
    assert_eq!(analysis.canonical_std_items.io_error_kind, Some(kind));
    let DefinitionKind::Enum(enumeration) = &program.definitions[kind].kind else {
        panic!("IoErrorKind must be an ordinary source enum")
    };
    let variants = enumeration.variants.clone();
    assert_eq!(
        variants
            .iter()
            .map(|variant| {
                program.definitions[*variant]
                    .name
                    .as_ref()
                    .expect("named IoErrorKind variant")
                    .as_str()
            })
            .collect::<Vec<_>>(),
        [
            "NotFound",
            "PermissionDenied",
            "AlreadyExists",
            "InvalidInput",
            "ConnectionRefused",
            "ConnectionReset",
            "TimedOut",
            "UnexpectedEof",
            "Closed",
            "Other",
        ]
    );

    let selected = definition_named(&program, &application_package, "application", "selected");
    assert_eq!(
        callable_return_definition(&program, &analysis, selected),
        kind
    );
    let targets = call_targets(&program, &analysis, selected);
    for variant_name in ["NotFound", "Other"] {
        let variant = definition_named(&program, &std_package, "std.io", variant_name);
        assert!(
            targets.contains(&CallTarget::EnumVariant(variant)),
            "{variant_name} did not resolve to its source variant: {targets:#?}"
        );
    }

    let inspect = definition_named(&program, &application_package, "application", "inspect");
    let DefinitionKind::Function(inspect) = &program.definitions[inspect].kind else {
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
            "pattern did not resolve to source variant {variant:?}: {resolutions:#?}"
        );
    }
}

#[test]
fn application_homonym_cannot_replace_prelude_io_error_kind() {
    let (program, analysis, std_package, application_package) = analyze_with_io(
        r"
pub enum IoErrorKind {
    Other
}

pub fn canonical() IoErrorKind {
    IoErrorKind.Other
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    let canonical_kind = definition_named(&program, &std_package, "std.io", "IoErrorKind");
    let forged_kind =
        definition_named(&program, &application_package, "application", "IoErrorKind");
    assert_eq!(
        analysis.canonical_std_items.io_error_kind,
        Some(canonical_kind)
    );
    assert_ne!(canonical_kind, forged_kind);

    let canonical = definition_named(&program, &application_package, "application", "canonical");
    assert_eq!(
        callable_return_definition(&program, &analysis, canonical),
        canonical_kind
    );
    let forged_variant = definition_named(&program, &application_package, "application", "Other");
    let other = definition_named(&program, &std_package, "std.io", "Other");
    let targets = call_targets(&program, &analysis, canonical);
    assert!(
        targets.contains(&CallTarget::EnumVariant(other)),
        "the enum-qualified canonical variant must keep its source identity: {targets:#?}"
    );
    assert!(
        !targets.contains(&CallTarget::EnumVariant(forged_variant)),
        "the application homonym replaced the canonical source variant: {targets:#?}"
    );
}

#[test]
fn application_cannot_import_the_private_output_primitive() {
    let file = FileId(0);
    let source = parse_with_file(
        file,
        r#"
import std.io.__write_stdout

pub fn emit() {
    __write_stdout("forbidden")
}
"#,
    );
    let package = PackageId::new("application", "0");
    let mut lowered = lower_package_files([PackageSourceUnit {
        file,
        package: package.clone(),
        module: ModuleName::new("application"),
        syntax: source.ast(),
    }]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    lowered.program.register_package(package, [], true);

    let analysis = analyze(&lowered.program);
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "UnknownName" && diagnostic.primary.file == file),
        "{:#?}",
        analysis.diagnostics
    );
}
