use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{DefId, DefinitionKind, PackageSourceUnit, Program, lower_package_files};
use loom_sema::{Analysis, BuiltinValue, CallTarget, analyze};
use loom_syntax::parse_with_file;

const FLOAT_SOURCE: &str = r"
import std.float.__from_int
import std.float.__to_int

pub enum FloatToIntError {
    NonFinite
    OutOfRange
}

pub fn from_int(value Int) Float {
    __from_int(value)
}

pub fn to_int(value Float) Result[Int, FloatToIntError] {
    let converted, status = __to_int(value)
    if status == 0 {
        return Ok(converted)
    }
    if status == 1 {
        return Err(FloatToIntError.NonFinite)
    }
    Err(FloatToIntError.OutOfRange)
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

#[test]
fn public_conversions_are_source_calls_and_only_the_wrapper_owns_the_primitives() {
    let std_file = FileId(0);
    let application_file = FileId(1);
    let float = parse_with_file(std_file, FLOAT_SOURCE);
    let application = parse_with_file(
        application_file,
        r"
import std.float.FloatToIntError
import std.float.from_int
import std.float.to_int

pub fn widen(value Int) Float {
    from_int(value)
}

pub fn truncate(value Float) Result[Int, FloatToIntError] {
    to_int(value)
}
",
    );
    assert!(float.diagnostics().is_empty(), "{:#?}", float.diagnostics());
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
            module: ModuleName::new("std.float"),
            syntax: float.ast(),
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
    let from_int = definition_named(&lowered.program, &std_package, "std.float", "from_int");
    let to_int = definition_named(&lowered.program, &std_package, "std.float", "to_int");
    let widen = definition_named(
        &lowered.program,
        &application_package,
        "application",
        "widen",
    );
    let truncate = definition_named(
        &lowered.program,
        &application_package,
        "application",
        "truncate",
    );
    assert!(
        call_targets(&lowered.program, &analysis, from_int)
            .contains(&CallTarget::Builtin(BuiltinValue::IntToFloat))
    );
    assert!(
        call_targets(&lowered.program, &analysis, to_int)
            .contains(&CallTarget::Builtin(BuiltinValue::FloatToIntStatus))
    );
    assert_eq!(
        call_targets(&lowered.program, &analysis, widen),
        vec![CallTarget::Function(from_int)]
    );
    assert_eq!(
        call_targets(&lowered.program, &analysis, truncate),
        vec![CallTarget::Function(to_int)]
    );
}

#[test]
fn application_cannot_import_private_float_primitives() {
    let file = FileId(0);
    let source = parse_with_file(
        file,
        r"
import std.float.__from_int
import std.float.__to_int

pub fn forbidden_int_to_float(value Int) Float {
    __from_int(value)
}

pub fn forbidden_float_to_int(value Float) (Int, Int) {
    __to_int(value)
}
",
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
fn numeric_conversion_remains_explicit() {
    let file = FileId(0);
    let source = parse_with_file(
        file,
        r"
fn implicit_int_to_float(value Int) Float {
    value
}

fn implicit_float_to_int(value Float) Int {
    value
}
",
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
            .filter(|diagnostic| diagnostic.code == "TypeMismatch")
            .count()
            >= 2,
        "{:#?}",
        analysis.diagnostics
    );
}
