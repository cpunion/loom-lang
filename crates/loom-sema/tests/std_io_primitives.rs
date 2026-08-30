use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{DefId, DefinitionKind, PackageSourceUnit, Program, lower_package_files};
use loom_sema::{Analysis, BuiltinValue, CallTarget, analyze};
use loom_syntax::parse_with_file;

const IO_SOURCE: &str = r#"
import std.io.__write_stdout

pub fn write(text Text) {
    __write_stdout(text)
}

pub fn write_line(text Text) {
    __write_stdout(text.concat("\n"))
}
"#;

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
fn public_output_resolves_to_source_and_only_the_wrapper_owns_the_primitive() {
    let std_file = FileId(0);
    let application_file = FileId(1);
    let io = parse_with_file(std_file, IO_SOURCE);
    let application = parse_with_file(
        application_file,
        r#"
import std.io.write

pub fn emit() {
    write("application")
}
"#,
    );
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
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    let write = definition_named(&lowered.program, &std_package, "std.io", "write");
    let emit = definition_named(
        &lowered.program,
        &application_package,
        "application",
        "emit",
    );
    assert_eq!(
        call_targets(&lowered.program, &analysis, write),
        vec![CallTarget::Builtin(BuiltinValue::StdoutWrite)]
    );
    assert_eq!(
        call_targets(&lowered.program, &analysis, emit),
        vec![CallTarget::Function(write)]
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
