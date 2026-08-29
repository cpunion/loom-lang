use loom_core::FileId;
use loom_hir::{SourceUnit, lower_files};
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fn analyze_source(source: &str) -> Vec<loom_core::Diagnostic> {
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
    analyze(&lowered.program).diagnostics
}

#[test]
fn ordinary_values_and_unit_may_be_explicitly_discarded() {
    let diagnostics = analyze_source(
        r#"
fn number() Int {
    42
}

fn discardValues() {
    discard number()
    discard "unused"
    discard Unit
}

fn omittedReturnIsUnit() {
    discard number()
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn an_omitted_unit_return_does_not_discard_its_tail_expression() {
    let diagnostics = analyze_source(
        r"
fn invalid() {
    42
}
",
    );
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].code, "TypeMismatch");
}

#[test]
fn discarding_a_diverging_block_does_not_require_a_return_value() {
    let diagnostics = analyze_source(
        r"
fn exits() {
    discard {
        return
    }
}
",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn a_bare_non_unit_statement_remains_an_error() {
    let diagnostics = analyze_source(
        r"
fn number() Int {
    42
}

fn bareValues() {
    42
    number()
    return
}
",
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "UnusedValue")
            .count(),
        2,
        "{diagnostics:#?}"
    );
}

#[test]
fn must_scope_obligations_cannot_be_discarded_through_wrappers() {
    let diagnostics = analyze_source(
        r"
fn direct(file File) {
    discard file
}

fn wrapped(value Result[Option[File], IoError]) {
    discard value
}
",
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "MustScopeRequiresScoped")
            .count(),
        2,
        "{diagnostics:#?}"
    );
}

#[test]
fn tasks_cannot_be_discarded_directly_or_through_wrappers() {
    let diagnostics = analyze_source(
        r"
record TaskBox[T] {
    task Task[T]
}

async fn work() Int {
    42
}

fn direct() {
    discard work()
}

fn wrapped(value Option[Task[Int]]) {
    discard value
}

fn listed(value List[Task[Int]]) {
    discard value
}

fn boxed(value TaskBox[Int]) {
    discard value
}
",
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "UnawaitedAsyncCall")
            .count(),
        4,
        "{diagnostics:#?}"
    );
}

#[test]
fn unconstrained_generic_values_cannot_be_discarded() {
    let diagnostics = analyze_source(
        r"
fn ignore[T](value T) {
    discard value
}
",
    );
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].code, "CannotDiscardUnknownType");
}

#[test]
fn bare_resource_and_task_statements_report_their_stronger_obligations() {
    let diagnostics = analyze_source(
        r"
async fn work() Int {
    42
}

fn bare(file File) {
    file
    work()
    return
}
",
    );
    let codes = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert_eq!(codes, ["MustScopeRequiresScoped", "UnawaitedAsyncCall"]);
    assert!(!codes.contains(&"UnusedValue"), "{diagnostics:#?}");
}

#[test]
fn dyn_erasure_rejects_hidden_resource_task_and_unknown_obligations() {
    let diagnostics = analyze_source(
        r#"
dyn concept Label {
    method label(self) Text
}

record Plain {}

record TaskBox {
    task Task[Int]
}

record ResourceBox {
    resource File
}

impl Label for Plain {
    method label(self) Text { "plain" }
}

impl Label for File {
    method label(self) Text { "file" }
}

impl Label for ResourceBox {
    method label(self) Text { "resource-box" }
}

fn erasePlain(value Plain) dyn Label {
    value
}

fn eraseTask(value Task[Int]) dyn Label {
    value
}

fn eraseFile(value File) dyn Label {
    value
}

fn eraseTaskBox(value TaskBox) dyn Label {
    value
}

fn eraseResourceBox(value ResourceBox) dyn Label {
    value
}

fn eraseGeneric[T: Label](value T) dyn Label {
    value
}

fn ignoreDyn(value dyn Label) {
    discard value
}
"#,
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "IllegalDynConversion")
            .count(),
        5,
        "{diagnostics:#?}"
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "IllegalDynConversion"),
        "{diagnostics:#?}"
    );
}
