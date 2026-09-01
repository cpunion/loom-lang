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
fn zero_payload_source_variants_are_values_not_calls() {
    let valid = analyze_source(
        r"enum Status[T] {
    Ready
}

fn ready() Status[Int] {
    Status.Ready
}
",
    );
    assert!(valid.is_empty(), "{valid:#?}");

    let invalid = analyze_source(
        r"enum Status {
    Ready
}

fn invalid() {
    discard Status.Ready()
    discard Status.Ready(1)
    discard Status.Ready[Int]()
}
",
    );
    assert_eq!(invalid.len(), 3, "{invalid:#?}");
    for diagnostic in invalid {
        assert_eq!(diagnostic.code, "TypeMismatch");
        assert_eq!(diagnostic.message, "value constructor is not callable");
    }
}

#[test]
fn source_variant_calls_honor_explicit_type_arguments() {
    let valid = analyze_source(
        r"enum Tagged[T] {
    Value(Int)
}

enum PartiallyTagged[A, B] {
    Value(B)
}

fn tagged() {
    discard Tagged.Value[Bool](1)
    discard PartiallyTagged.Value[Bool](1)
}
",
    );
    assert!(valid.is_empty(), "{valid:#?}");

    let invalid = analyze_source(
        r"enum Tagged[T] {
    Value(T)
}

fn invalid() {
    discard Tagged.Value[Int](true)
}
",
    );
    assert!(
        invalid.iter().any(|diagnostic| {
            diagnostic.code == "TypeMismatch"
                && diagnostic.message.contains("expected Int, found Bool")
        }),
        "{invalid:#?}"
    );
}

#[test]
fn source_variant_call_type_argument_arity_is_checked() {
    let diagnostics = analyze_source(
        r"enum Tagged[T] {
    Value(Int)
}

fn invalid() {
    discard Tagged.Value[Bool, Text](1)
}
",
    );
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].code, "TypeMismatch");
    assert_eq!(
        diagnostics[0].message,
        "too many explicit generic arguments"
    );
}
