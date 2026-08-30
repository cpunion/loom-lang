use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{DefId, DefinitionKind, PackageSourceUnit, Program, lower_package_files};
use loom_sema::{Analysis, BuiltinValue, CallTarget, Resolution, Signature, TyData, analyze};
use loom_syntax::parse_with_file;

const IO_SOURCE: &str = include_str!("../../../library/std/io/io.loom");
const UNAUTHORIZED_ACCESSOR_SOURCE: &str = r"
import std.io.__error_kind
import std.io.__error_message

fn steal_kind(error IoError) IoErrorKind {
    __error_kind(error)
}

fn steal_message(error IoError) Text {
    __error_message(error)
}

record Counterfeit {}

impl Counterfeit {
    method kind(self) IoErrorKind {
        __error_kind(self)
    }

    method message(self) Text {
        __error_message(self)
    }
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
    let body = match &program.definitions[definition].kind {
        DefinitionKind::Function(function) => function.body,
        DefinitionKind::Method(method) => method.body.expect("method has a body"),
        _ => panic!("definition is not callable"),
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

fn assert_no_private_io_error_access(program: &Program, analysis: &Analysis, file: FileId) {
    for (definition, item) in program.definitions.iter() {
        if program
            .source_map
            .definition(definition)
            .is_none_or(|span| span.file != file)
            || !matches!(
                item.kind,
                DefinitionKind::Function(_) | DefinitionKind::Method(_)
            )
        {
            continue;
        }
        assert!(
            call_targets(program, analysis, definition)
                .iter()
                .all(|target| !matches!(
                    target,
                    CallTarget::Builtin(BuiltinValue::IoErrorKind | BuiltinValue::IoErrorMessage)
                ))
        );
    }
}

fn callable_parameter_definition(analysis: &Analysis, definition: DefId) -> DefId {
    let Some(Signature::Callable(signature)) = analysis.typed.signatures.get(definition) else {
        panic!("definition is not callable")
    };
    let [(_, parameter)] = signature.params.as_slice() else {
        panic!("callable must have exactly one parameter")
    };
    let TyData::Nominal {
        definition,
        arguments,
    } = analysis.typed.types.data(*parameter)
    else {
        panic!("callable parameter is not nominal")
    };
    assert!(arguments.is_empty(), "IoError must not be generic");
    *definition
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
fn io_error_is_a_protected_source_record_with_ordinary_accessor_methods() {
    let (program, analysis, std_package, application_package) = analyze_with_io(
        r"
pub fn inspect(error IoError) {
    let kind = error.kind()
    let message = error.message()
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );

    let error = definition_named(&program, &std_package, "std.io", "IoError");
    assert_eq!(analysis.canonical_std_items.io_error, Some(error));
    let DefinitionKind::Record(record) = &program.definitions[error].kind else {
        panic!("IoError must be an ordinary source record")
    };
    assert!(record.fields.is_empty(), "IoError must remain protected");

    let kind = definition_named(&program, &std_package, "std.io", "kind");
    let message = definition_named(&program, &std_package, "std.io", "message");
    assert_eq!(
        call_targets(&program, &analysis, kind),
        vec![CallTarget::Builtin(BuiltinValue::IoErrorKind)]
    );
    assert_eq!(
        call_targets(&program, &analysis, message),
        vec![CallTarget::Builtin(BuiltinValue::IoErrorMessage)]
    );
    assert_eq!(
        callable_return_definition(&program, &analysis, kind),
        analysis
            .canonical_std_items
            .io_error_kind
            .expect("canonical IoErrorKind")
    );
    let Some(Signature::Callable(message_signature)) = analysis.typed.signatures.get(message)
    else {
        panic!("message must be callable")
    };
    assert_eq!(
        analysis.typed.types.data(message_signature.return_ty),
        &TyData::Builtin(loom_sema::BuiltinType::Text)
    );

    let inspect = definition_named(&program, &application_package, "application", "inspect");
    assert_eq!(callable_parameter_definition(&analysis, inspect), error);
    let targets = call_targets(&program, &analysis, inspect);
    assert!(
        targets.contains(&CallTarget::InherentMethod(kind)),
        "{targets:#?}"
    );
    assert!(
        targets.contains(&CallTarget::InherentMethod(message)),
        "{targets:#?}"
    );
    assert!(targets.iter().all(|target| !matches!(
        target,
        CallTarget::Builtin(BuiltinValue::IoErrorKind | BuiltinValue::IoErrorMessage)
    )));
}

#[test]
fn canonical_io_error_cannot_be_constructed_or_compared() {
    let (program, analysis, std_package, application_package) = analyze_with_io(
        r"
pub record IoError {}

fn construct() {
    let short = IoError {}
    let qualified = std.io.IoError {}
}

fn compare(left IoError, right IoError) Bool {
    left == right
}
",
    );
    assert_eq!(
        analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.code == "TypeMismatch"
                    && diagnostic
                        .message
                        .contains("cannot be constructed directly")
            })
            .count(),
        2,
        "{:#?}",
        analysis.diagnostics
    );
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "InvalidGenericOperation"),
        "{:#?}",
        analysis.diagnostics
    );
    let canonical = definition_named(&program, &std_package, "std.io", "IoError");
    let homonym = definition_named(&program, &application_package, "application", "IoError");
    assert_eq!(analysis.canonical_std_items.io_error, Some(canonical));
    assert_ne!(canonical, homonym);
    let compare = definition_named(&program, &application_package, "application", "compare");
    let Some(Signature::Callable(signature)) = analysis.typed.signatures.get(compare) else {
        panic!("compare must be callable")
    };
    assert!(signature.params.iter().all(|(_, ty)| {
        matches!(
            analysis.typed.types.data(*ty),
            TyData::Nominal {
                definition,
                arguments,
            } if *definition == canonical && arguments.is_empty()
        )
    }));
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

#[test]
fn application_cannot_import_private_io_error_accessors() {
    let (program, analysis, _, _) = analyze_with_io(UNAUTHORIZED_ACCESSOR_SOURCE);
    assert_eq!(
        analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.code == "UnknownName" && diagnostic.primary.file == FileId(1)
            })
            .count(),
        6,
        "{:#?}",
        analysis.diagnostics
    );
    assert_no_private_io_error_access(&program, &analysis, FileId(1));
}

#[test]
fn private_io_error_accessors_require_the_exact_source_methods() {
    let io_file = FileId(0);
    let hostile_file = FileId(1);
    let io = parse_with_file(io_file, IO_SOURCE);
    let hostile = parse_with_file(hostile_file, UNAUTHORIZED_ACCESSOR_SOURCE);
    assert!(io.diagnostics().is_empty(), "{:#?}", io.diagnostics());
    assert!(
        hostile.diagnostics().is_empty(),
        "{:#?}",
        hostile.diagnostics()
    );
    let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: io_file,
            package: std_package.clone(),
            module: ModuleName::new("std.io"),
            syntax: io.ast(),
        },
        PackageSourceUnit {
            file: hostile_file,
            package: std_package.clone(),
            module: ModuleName::new("std.io"),
            syntax: hostile.ast(),
        },
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    lowered
        .program
        .register_package(std_package.clone(), [], false);
    let analysis = analyze(&lowered.program);
    assert_eq!(
        analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.code == "UnknownName" && diagnostic.primary.file == hostile_file
            })
            .count(),
        4,
        "{:#?}",
        analysis.diagnostics
    );
    assert_no_private_io_error_access(&lowered.program, &analysis, hostile_file);
}
