use loom_core::FileId;
use loom_hir::{SourceUnit, lower_files};
use loom_lowering::lower_to_mir;
use loom_mir::{RequirementType, StatementKind, Type};
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fn compile_and_validate(source: &str) -> loom_mir::CheckedProgram {
    let parsed = parse_with_file(FileId(0), source);
    assert!(
        parsed.diagnostics().is_empty(),
        "syntax diagnostics: {:#?}",
        parsed.diagnostics()
    );
    let lowered = lower_files([SourceUnit {
        file: FileId(0),
        syntax: parsed.ast(),
    }]);
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
        r"module returns

record R {}

pub fn omitted() { return }
async fn omittedAsync() {}
test fn omittedTest() { return }
fn explicit() Unit { Unit }
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

    assert_eq!(program.functions.len(), 7);
    assert!(
        program
            .functions
            .iter()
            .all(|function| function.return_ty == Type::Unit)
    );
    assert_eq!(program.requirements.len(), 1);
    assert_eq!(program.requirements[0].return_ty, RequirementType::Unit);
    for name in [
        "omitted",
        "omittedAsync",
        "omittedTest",
        "explicit",
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
