use loom_core::FileId;
use loom_hir::{SourceUnit, lower_files};
use loom_sema::{Analysis, analyze};
use loom_syntax::parse_with_file;

fn analyze_program(source: &str) -> Analysis {
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
    analyze(&lowered.program)
}

fn diagnostics(source: &str) -> Vec<loom_core::Diagnostic> {
    analyze_program(source).diagnostics
}

#[test]
fn projected_inout_cannot_cross_an_external_or_nested_invariant() {
    let diagnostics = diagnostics(
        r"
record Counter {
    value Int
}

impl Counter {
    method decrement(mut self) {
        self.value = self.value - 1
    }
}

record Positive {
    counter Counter
    invariant self.counter.value >= 0
}

record Holder {
    value Positive
}

impl Holder {
    method bypassNestedInvariant(mut self) {
        self.value.counter.decrement()
    }
}

fn bypassExternalInvariant() {
    var value = Positive { counter = Counter { value = 0 } }
    value.counter.decrement()
}
",
    );
    let invariant_interior = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "InvariantInteriorMutation")
        .count();
    assert_eq!(invariant_interior, 2, "{diagnostics:#?}");
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "InvariantInteriorMutation"),
        "{diagnostics:#?}"
    );
}

#[test]
fn root_and_unprotected_projected_inout_remain_available() {
    let diagnostics = diagnostics(
        r"
record Counter {
    value Int
}

impl Counter {
    method increment(mut self) {
        self.value = self.value + 1
    }
}

record Positive {
    counter Counter
    invariant self.counter.value >= 0
}

impl Positive {
    method increment(mut self) {
        self.counter.increment()
        assert self.counter.value >= 0
    }
}

record Plain {
    counter Counter
}

fn valid() {
    var positive = Positive { counter = Counter { value = 0 } }
    positive.increment()

    var plain = Plain { counter = Counter { value = 0 } }
    plain.counter.increment()
}
",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn owning_projected_inout_marks_the_receiver_invariant_dirty() {
    let diagnostics = diagnostics(
        r"
record Counter {
    value Int
}

impl Counter {
    method decrement(mut self) {
        self.value = self.value - 1
    }
}

record Positive {
    counter Counter
    invariant self.counter.value >= 0
}

impl Positive {
    method observe(self) {
    }

    method unchecked(mut self) {
        self.counter.decrement()
        self.observe()
    }

    method checked(mut self) {
        self.counter.decrement()
        assert self.counter.value >= 0
        self.observe()
    }
}
",
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "InvariantIsolationViolation")
            .count(),
        1,
        "{diagnostics:#?}"
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "InvariantIsolationViolation"),
        "{diagnostics:#?}"
    );
}
