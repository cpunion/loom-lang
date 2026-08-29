use loom_core::FileId;
use loom_hir::{DefinitionKind, Program, SourceUnit, lower_files};
use loom_sema::{Analysis, BuiltinType, Signature, TyData, analyze};
use loom_syntax::parse_with_file;

fn analyze_source(source: &str) -> (Program, Analysis) {
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
    let program = lowered.program;
    let analysis = analyze(&program);
    (program, analysis)
}

#[test]
fn every_omitted_callable_return_resolves_to_unit() {
    let (program, analysis) = analyze_source(
        r"record R {}

fn private() {}
pub fn public() { return }
async fn asynchronous() {}
pub async fn publicAsynchronous() { return }
test fn omittedTest() {}
fn anotherOmitted() { return }
pub fn contracted(flag Bool)
    requires flag
    ensures true
{
    return
}

impl R {
    method privateMethod(self) {}
    pub method publicMethod(self) { return }
}

concept C {
    method required(self)
    static method requiredStatic()
}

impl C for R {
    method required(self) {}
    static method requiredStatic() { return }
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "semantic diagnostics: {:#?}",
        analysis.diagnostics
    );

    let mut callable_count = 0;
    for (definition, item) in program.definitions.iter() {
        if !matches!(
            item.kind,
            DefinitionKind::Function(_) | DefinitionKind::Test(_) | DefinitionKind::Method(_)
        ) {
            continue;
        }
        let Some(Signature::Callable(signature)) = analysis.typed.signatures.get(definition) else {
            panic!("missing callable signature for {definition:?}");
        };
        assert_eq!(
            analysis.typed.types.data(signature.return_ty),
            &TyData::Builtin(BuiltinType::Unit),
            "callable {:?} did not resolve to Unit",
            item.name
        );
        callable_count += 1;
    }
    assert_eq!(callable_count, 13);
}

#[test]
fn calling_an_omitted_async_function_produces_task_of_unit() {
    let (_, analysis) = analyze_source(
        r"async fn child() {}

async fn parent() {
    child().await
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "semantic diagnostics: {:#?}",
        analysis.diagnostics
    );

    assert!(analysis.typed.bodies.iter().any(|(_, body)| {
        body.expression_types.iter().any(|(_, ty)| {
            let TyData::Task(output) = analysis.typed.types.data(*ty) else {
                return false;
            };
            analysis.typed.types.data(*output) == &TyData::Builtin(BuiltinType::Unit)
        })
    }));
}

#[test]
fn omitted_return_is_not_inferred_from_a_value() {
    let (_, analysis) = analyze_source("fn inferred() { 1 }\n");
    assert_eq!(analysis.diagnostics.len(), 1, "{:#?}", analysis.diagnostics);
    assert_eq!(analysis.diagnostics[0].code, "TypeMismatch");

    let (_, analysis) = analyze_source("fn returned() { return 1 }\n");
    assert_eq!(analysis.diagnostics.len(), 1, "{:#?}", analysis.diagnostics);
    assert_eq!(analysis.diagnostics[0].code, "TypeMismatch");
}

#[test]
fn bare_return_requires_a_unit_signature() {
    let (_, analysis) = analyze_source("fn invalid() Int { return }\n");
    assert_eq!(analysis.diagnostics.len(), 1, "{:#?}", analysis.diagnostics);
    assert_eq!(analysis.diagnostics[0].code, "TypeMismatch");
    assert!(analysis.diagnostics[0].message.contains("bare return"));
}

#[test]
fn unit_must_remain_explicit_inside_result_and_task_types() {
    let (_, analysis) = analyze_source(
        r"enum E { Failed }

concept ExplicitCarriers {
    method outcome(self) Result[Unit, E]
    method task(self) Task[Unit]
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "semantic diagnostics: {:#?}",
        analysis.diagnostics
    );

    let (_, analysis) = analyze_source(
        r"concept MissingCarrierArguments {
    method outcome(self) Result
    method task(self) Task
}
",
    );
    assert_eq!(analysis.diagnostics.len(), 2, "{:#?}", analysis.diagnostics);
    assert!(
        analysis
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "TypeMismatch")
    );
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("`Result`"))
    );
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("`Task`"))
    );
}
