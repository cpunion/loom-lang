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
fn while_and_loop_control_have_the_minimal_scoped_rules() {
    let diagnostics = analyze_source(
        r"
fn valid(flag Bool) {
    while flag {
        continue
    }
    for index in 0..2 {
        discard index
        break
    }
    defer {
        while true {
            break
        }
    }
}

fn outside() {
    break
    continue
}

fn cleanupCannotEscape() {
    while true {
        defer {
            break
        }
        break
    }
}

fn boolConditionOnly() {
    while 1 {}
}
",
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "LoopControlOutsideLoop")
            .count(),
        2,
        "{diagnostics:#?}"
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "LoopControlFromCleanup")
            .count(),
        1,
        "{diagnostics:#?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "TypeMismatch"),
        "{diagnostics:#?}"
    );
}
