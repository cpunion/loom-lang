use loom_core::FileId;
use loom_hir::{Program, SourceUnit, lower_files};
use loom_sema::{Analysis, CallTarget, StandardLibraryItem, analyze};
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
    let analysis = analyze(&lowered.program);
    (lowered.program, analysis)
}

fn standard_items(analysis: &Analysis) -> Vec<StandardLibraryItem> {
    let mut items = analysis
        .typed
        .bodies
        .values()
        .flat_map(|body| body.calls.values())
        .filter_map(|call| match call.target {
            CallTarget::StandardLibrary(item) => Some(item),
            _ => None,
        })
        .collect::<Vec<_>>();
    items.sort_unstable();
    items
}

#[test]
fn canonical_task_members_resolve_to_stable_standard_library_items() {
    let (_, analysis) = analyze_source(
        r"
module canonical_task

async fn child() Int { 1 }

pub async fn main() Unit {
    discard Task.sleep(0).await
    discard Task.all(child()).await
    discard Task.settled(child()).await
    discard Task.any(child()).await
    discard Task.race(child()).await
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    assert_eq!(
        standard_items(&analysis),
        [
            StandardLibraryItem::TaskSleep,
            StandardLibraryItem::TaskAll,
            StandardLibraryItem::TaskSettled,
            StandardLibraryItem::TaskAny,
            StandardLibraryItem::TaskRace,
        ]
    );
}

#[test]
fn local_and_parameter_named_task_keep_ordinary_method_identity() {
    let (_, analysis) = analyze_source(
        r"
module shadowed_task

record Scheduler {}

impl Scheduler {
    method all(self, value Int) Int { value }
    method settled(self, value Int) Int { value }
    method any(self, value Int) Int { value }
    method race(self, value Int) Int { value }
    method sleep(self, value Int) Int { value }
}

fn localReceiver() Unit {
    let Task = Scheduler {}
    discard Task.all(1)
    discard Task.settled(2)
    discard Task.any(3)
    discard Task.race(4)
    discard Task.sleep(5)
}

fn parameterReceiver(Task Scheduler) Unit {
    discard Task.all(6)
    discard Task.settled(7)
    discard Task.any(8)
    discard Task.race(9)
    discard Task.sleep(10)
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    assert!(standard_items(&analysis).is_empty());
    let inherent_calls = analysis
        .typed
        .bodies
        .values()
        .flat_map(|body| body.calls.values())
        .filter(|call| matches!(call.target, CallTarget::InherentMethod(_)))
        .count();
    assert_eq!(inherent_calls, 10);
}

#[test]
fn ambiguous_value_named_task_never_falls_back_to_the_standard_catalog() {
    let (_, analysis) = analyze_source(
        r"
module ambiguous_task

record Scheduler {}

fn Task() Scheduler { Scheduler {} }
fn Task() Scheduler { Scheduler {} }

fn useTask() Unit {
    discard Task.any(1)
}
",
    );
    assert!(analysis.has_errors());
    assert!(standard_items(&analysis).is_empty());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "DuplicateDeclaration"),
        "{:#?}",
        analysis.diagnostics
    );
    assert!(
        analysis
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "TaskJoinRequiresTasks"),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
fn invalid_private_task_import_never_falls_back_to_the_standard_catalog() {
    let library = parse_with_file(
        FileId(0),
        r"
module library

record Scheduler {}
fn Task() Scheduler { Scheduler {} }
",
    );
    let application = parse_with_file(
        FileId(1),
        r"
module application

import library.Task

fn useTask() Unit {
    discard Task.any(1)
}
",
    );
    assert!(library.diagnostics().is_empty());
    assert!(application.diagnostics().is_empty());
    let lowered = lower_files([
        SourceUnit {
            file: FileId(0),
            syntax: library.ast(),
        },
        SourceUnit {
            file: FileId(1),
            syntax: application.ast(),
        },
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    let analysis = analyze(&lowered.program);
    assert!(analysis.has_errors());
    assert!(standard_items(&analysis).is_empty());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "NameNotVisible"),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
fn imported_enum_constructors_still_use_the_type_namespace() {
    let library = parse_with_file(
        FileId(0),
        r"
module library

pub enum CheckoutError {
    OutOfStock
    Rejected
}
",
    );
    let application = parse_with_file(
        FileId(1),
        r"
module application

import library.CheckoutError

fn rejected() CheckoutError { CheckoutError.Rejected }
fn outOfStock() CheckoutError { CheckoutError.OutOfStock }
",
    );
    assert!(library.diagnostics().is_empty());
    assert!(application.diagnostics().is_empty());
    let lowered = lower_files([
        SourceUnit {
            file: FileId(0),
            syntax: library.ast(),
        },
        SourceUnit {
            file: FileId(1),
            syntax: application.ast(),
        },
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    let analysis = analyze(&lowered.program);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    assert!(standard_items(&analysis).is_empty());
    let variant_calls = analysis
        .typed
        .bodies
        .values()
        .flat_map(|body| body.calls.values())
        .filter(|call| matches!(call.target, CallTarget::EnumVariant(_)))
        .count();
    assert_eq!(variant_calls, 2);
}

#[test]
fn user_type_named_task_cannot_forge_a_standard_item_identity() {
    let (_, analysis) = analyze_source(
        r"
module forged_task_type

record Task {}

fn useTask() Unit {
    discard Task.any(1)
}
",
    );
    assert!(analysis.has_errors());
    assert!(standard_items(&analysis).is_empty());
}

#[test]
fn generic_parameter_named_task_cannot_forge_a_standard_item_identity() {
    let (_, analysis) = analyze_source(
        r"
module forged_task_parameter

async fn child() Int { 1 }

async fn useTask[Task]() Unit {
    discard Task.any(child()).await
}
",
    );
    assert!(analysis.has_errors());
    assert!(standard_items(&analysis).is_empty());
    assert!(
        analysis
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "TaskJoinRequiresTasks"),
        "{:#?}",
        analysis.diagnostics
    );
}
