use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{PackageSourceUnit, lower_package_files};
use loom_lowering::lower_to_mir;
use loom_mir::{RequirementType, StatementKind, Type};
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fn compile_and_validate(source: &str) -> loom_mir::CheckedProgram {
    let application_file = FileId(0);
    let io_file = FileId(1);
    let parsed = parse_with_file(application_file, source);
    let io = parse_with_file(io_file, include_str!("../../../library/std/io/io.loom"));
    assert!(
        parsed.diagnostics().is_empty() && io.diagnostics().is_empty(),
        "syntax diagnostics: application={:#?} io={:#?}",
        parsed.diagnostics(),
        io.diagnostics()
    );
    let root = PackageId::new("implicit-unit-test", "0");
    let std = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: application_file,
            package: root.clone(),
            module: ModuleName::new("implicit_unit_test"),
            syntax: parsed.ast(),
        },
        PackageSourceUnit {
            file: io_file,
            package: std.clone(),
            module: ModuleName::new("std.io"),
            syntax: io.ast(),
        },
    ]);
    lowered.program.register_package(std.clone(), [], false);
    lowered
        .program
        .register_package(root, [(Name::new("std"), std)], true);
    assert!(
        lowered.diagnostics.is_empty(),
        "HIR diagnostics: {:#?}",
        lowered.diagnostics
    );
    let analysis = analyze(&lowered.program);
    assert!(
        analysis.diagnostics.is_empty(),
        "semantic diagnostics: {:#?}",
        analysis.diagnostics
    );
    lower_to_mir(&lowered.program, &analysis)
        .unwrap_or_else(|failure| panic!("MIR diagnostics: {:#?}", failure.diagnostics()))
}

#[test]
fn omitted_returns_lower_to_unit_without_return_inference() {
    let program = compile_and_validate(
        r"record R {}

pub fn omitted() { return }
async fn omittedAsync() {}
test fn omittedTest() { return }
fn anotherOmitted() {}
pub fn contracted(flag Bool)
    requires flag
    ensures true
{
    return
}

impl R {
    pub method inherent(self) {}
}

concept C {
    method required(self)
}

impl C for R {
    method required(self) { return }
}
",
    );

    let functions = program
        .functions
        .iter()
        .filter(|function| function.name.starts_with("implicit_unit_test."))
        .collect::<Vec<_>>();
    assert_eq!(functions.len(), 7);
    assert!(
        functions
            .iter()
            .all(|function| function.return_ty == Type::Unit)
    );
    assert_eq!(program.requirements.len(), 1);
    assert_eq!(program.requirements[0].return_ty, RequirementType::Unit);
    for name in [
        "omitted",
        "omittedAsync",
        "omittedTest",
        "anotherOmitted",
        "contracted",
        "inherent",
    ] {
        let function = program
            .functions
            .iter()
            .find(|function| function.name.rsplit('.').next() == Some(name))
            .unwrap_or_else(|| panic!("missing lowered function {name}"));
        assert_eq!(function.witness_prefix_count, 0, "{name}");
    }
    assert!(program.functions.iter().any(|function| {
        function
            .body
            .statements
            .iter()
            .any(|statement| matches!(statement.kind, StatementKind::Return(None)))
    }));
    let contracted = program
        .functions
        .iter()
        .find(|function| function.name.rsplit('.').next() == Some("contracted"))
        .expect("contracted function");
    assert_eq!(contracted.call_plan.requires.len(), 1);
}
