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

fn analyze_source(source: &str) -> Vec<loom_core::Diagnostic> {
    analyze_program(source).diagnostics
}

fn inout_alias_count(diagnostics: &[loom_core::Diagnostic]) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "InoutAliasConflict")
        .count()
}

#[test]
fn list_add_reservation_allows_reads_and_sibling_mutation() {
    let diagnostics = analyze_source(
        r"
module inout_list_valid

record Lists {
    left List[Unit]
    right List[Unit]
}

fn valid() Unit {
    var values = List[Int]()
    values.add(values.length())

    var lists = Lists {
        left = List[Unit](),
        right = List[Unit](),
    }
    lists.left.add(lists.right.add(Unit))
    Unit
}
",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn list_add_rejects_overlapping_nested_mutation() {
    let diagnostics = analyze_source(
        r"
module inout_list_invalid

fn invalid() Unit {
    var values = List[Unit]()
    values.add(values.add(Unit))
    Unit
}
",
    );
    assert_eq!(inout_alias_count(&diagnostics), 1, "{diagnostics:#?}");
}

#[test]
fn inherent_inout_scope_covers_nested_calls_assignments_and_dyn_adaptation() {
    let diagnostics = analyze_source(
        r"
module inout_inherent

dyn concept Touch {
    method touch(mut self) Unit
}

record Cell {
    value Int
}

impl Touch for Cell {
    method touch(mut self) Unit {
        self.value = self.value + 1
    }
}

impl Cell {
    method read(self) Int {
        self.value
    }

    method reset(mut self) Int {
        self.value = 0
        self.value
    }

    method consumeInt(mut self, value Int) Unit {
        self.value = value
    }

    method consumeUnit(mut self, value Unit) Unit {
        self.value = self.value + 1
    }

    method consumeTouch(mut self, other Touch) Unit {
        other.touch()
    }
}

fn valid() Unit {
    var cell = Cell { value = 1 }
    cell.consumeInt(cell.read())
    Unit
}

fn invalidNested() Unit {
    var cell = Cell { value = 1 }
    cell.consumeInt(cell.reset())
    Unit
}

fn invalidAssign() Unit {
    var cell = Cell { value = 1 }
    cell.consumeUnit(if true { cell = Cell { value = 2 } } else { Unit })
    Unit
}

fn invalidMutableView() Unit {
    var cell = Cell { value = 1 }
    cell.consumeTouch(cell)
    Unit
}
",
    );
    assert_eq!(inout_alias_count(&diagnostics), 3, "{diagnostics:#?}");
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "InoutAliasConflict"),
        "{diagnostics:#?}"
    );
}

#[test]
fn static_and_dynamic_concept_calls_reserve_the_mutable_receiver() {
    let diagnostics = analyze_source(
        r"
module inout_concepts

record Counter {
    value Int
}

dyn concept CounterOps {
    method read(self) Int
    method reset(mut self) Int
    method consume(mut self, value Int) Unit
}

impl CounterOps for Counter {
    method read(self) Int {
        self.value
    }

    method reset(mut self) Int {
        self.value = 0
        self.value
    }

    method consume(mut self, value Int) Unit {
        self.value = value
    }
}

fn validStatic() Unit {
    var counter = Counter { value = 1 }
    counter.consume(counter.read())
    Unit
}

fn invalidStatic() Unit {
    var counter = Counter { value = 1 }
    counter.consume(counter.reset())
    Unit
}

fn invalidDynamic(counter dyn CounterOps) Unit {
    counter.consume(counter.reset())
    Unit
}
",
    );
    assert_eq!(inout_alias_count(&diagnostics), 2, "{diagnostics:#?}");
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "InoutAliasConflict"),
        "{diagnostics:#?}"
    );
}

#[test]
fn inout_reservations_do_not_consume_serialized_view_tokens() {
    let analysis = analyze_program(
        r"
module inout_token_stability

record Cell {
    value Int
}

dyn concept Touch {
    method touch(mut self) Unit
}

impl Touch for Cell {
    method touch(mut self) Unit {
        self.value = self.value + 1
    }
}

fn touch(value Touch) Unit {
    value.touch()
}

fn valid() Unit {
    var values = List[Unit]()
    values.add(Unit)

    var cell = Cell { value = 1 }
    touch(cell)
    Unit
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    let tokens = analysis
        .typed
        .bodies
        .values()
        .flat_map(|body| body.views.values().map(|view| view.token.0))
        .collect::<Vec<_>>();
    assert_eq!(tokens, vec![0]);
}
