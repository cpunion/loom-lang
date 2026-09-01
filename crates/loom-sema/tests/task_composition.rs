use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{PackageSourceUnit, Program, SourceUnit, lower_files, lower_package_files};
use loom_sema::{Analysis, BuiltinValue, CallTarget, TaskIntrinsic, analyze};
use loom_syntax::parse_with_file;

fn analyze_source(source: &str) -> (Program, Analysis) {
    let parsed = parse_with_file(FileId(0), source);
    let task = parse_with_file(
        FileId(1),
        include_str!("../../../library/std/task/task.loom"),
    );
    assert!(
        parsed.diagnostics().is_empty() && task.diagnostics().is_empty(),
        "syntax diagnostics: application={:#?} task={:#?}",
        parsed.diagnostics(),
        task.diagnostics()
    );
    let root = PackageId::standalone();
    let std = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: FileId(0),
            package: root.clone(),
            module: ModuleName::new("standalone"),
            syntax: parsed.ast(),
        },
        PackageSourceUnit {
            file: FileId(1),
            package: std.clone(),
            module: ModuleName::new("std.task"),
            syntax: task.ast(),
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
    (lowered.program, analysis)
}

fn task_intrinsics(analysis: &Analysis) -> Vec<TaskIntrinsic> {
    let mut intrinsics = analysis
        .typed
        .bodies
        .values()
        .flat_map(|body| body.calls.values())
        .filter_map(|call| match call.target {
            CallTarget::TaskIntrinsic(intrinsic) => Some(intrinsic),
            _ => None,
        })
        .collect::<Vec<_>>();
    intrinsics.sort_unstable();
    intrinsics
}

#[test]
fn sleep_resolves_to_source_while_joins_remain_task_intrinsics() {
    let (program, analysis) = analyze_source(
        r"
async fn child() Int { 1 }

pub async fn main() {
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
        task_intrinsics(&analysis),
        [
            TaskIntrinsic::All,
            TaskIntrinsic::Settled,
            TaskIntrinsic::Any,
            TaskIntrinsic::Race,
        ]
    );
    let source_sleep = analysis
        .typed
        .bodies
        .values()
        .flat_map(|body| body.calls.values())
        .filter(|call| {
            matches!(
                call.target,
                CallTarget::InherentMethod(definition)
                if program.definitions[definition]
                    .name
                    .as_ref()
                    .is_some_and(|name| name.as_str() == "sleep")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(source_sleep.len(), 1);
    assert!(source_sleep[0].receiver.is_none());
    assert!(analysis.typed.bodies.values().any(|body| {
        body.calls
            .values()
            .any(|call| call.target == CallTarget::Builtin(BuiltinValue::TaskSleep))
    }));
}

#[test]
fn local_and_parameter_named_task_keep_ordinary_method_identity() {
    let (_, analysis) = analyze_source(
        r"
record Scheduler {}

impl Scheduler {
    method all(self, value Int) Int { value }
    method settled(self, value Int) Int { value }
    method any(self, value Int) Int { value }
    method race(self, value Int) Int { value }
    method sleep(self, value Int) Int { value }
}

fn localReceiver() {
    let Task = Scheduler {}
    discard Task.all(1)
    discard Task.settled(2)
    discard Task.any(3)
    discard Task.race(4)
    discard Task.sleep(5)
}

fn parameterReceiver(Task Scheduler) {
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
    assert!(task_intrinsics(&analysis).is_empty());
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
fn ambiguous_value_named_task_never_falls_back_to_task_intrinsics() {
    let (_, analysis) = analyze_source(
        r"
record Scheduler {}

fn Task() Scheduler { Scheduler {} }
fn Task() Scheduler { Scheduler {} }

fn useTask() {
    discard Task.any(1)
}
",
    );
    assert!(analysis.has_errors());
    assert!(task_intrinsics(&analysis).is_empty());
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
fn invalid_private_task_import_never_falls_back_to_task_intrinsics() {
    let library = parse_with_file(
        FileId(0),
        r"
record Scheduler {}
fn Task() Scheduler { Scheduler {} }
",
    );
    let application = parse_with_file(
        FileId(1),
        r"
import library.Task

fn useTask() {
    discard Task.any(1)
}
",
    );
    assert!(library.diagnostics().is_empty());
    assert!(application.diagnostics().is_empty());
    let library_package = PackageId::new("library", "0");
    let application_package = PackageId::new("application", "0");
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: FileId(0),
            package: library_package.clone(),
            module: ModuleName::new("library"),
            syntax: library.ast(),
        },
        PackageSourceUnit {
            file: FileId(1),
            package: application_package.clone(),
            module: ModuleName::new("application"),
            syntax: application.ast(),
        },
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    lowered
        .program
        .register_package(library_package.clone(), [], false);
    lowered.program.register_package(
        application_package,
        [(Name::new("library"), library_package)],
        true,
    );
    let analysis = analyze(&lowered.program);
    assert!(analysis.has_errors());
    assert!(task_intrinsics(&analysis).is_empty());
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
pub enum CheckoutError {
    OutOfStock
    Rejected
}
",
    );
    let application = parse_with_file(
        FileId(1),
        r"
import library.CheckoutError

fn rejected() CheckoutError { CheckoutError.Rejected }
fn outOfStock() CheckoutError { CheckoutError.OutOfStock }
",
    );
    assert!(library.diagnostics().is_empty());
    assert!(application.diagnostics().is_empty());
    let library_package = PackageId::new("library", "0");
    let application_package = PackageId::new("application", "0");
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: FileId(0),
            package: library_package.clone(),
            module: ModuleName::new("library"),
            syntax: library.ast(),
        },
        PackageSourceUnit {
            file: FileId(1),
            package: application_package.clone(),
            module: ModuleName::new("application"),
            syntax: application.ast(),
        },
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    lowered
        .program
        .register_package(library_package.clone(), [], false);
    lowered.program.register_package(
        application_package,
        [(Name::new("library"), library_package)],
        true,
    );
    let analysis = analyze(&lowered.program);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    assert!(task_intrinsics(&analysis).is_empty());
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
fn user_type_named_task_cannot_forge_a_task_intrinsic() {
    let (_, analysis) = analyze_source(
        r"
record Task {}

fn useTask() {
    discard Task.any(1)
}
",
    );
    assert!(analysis.has_errors());
    assert!(task_intrinsics(&analysis).is_empty());
}

#[test]
fn generic_parameter_named_task_cannot_forge_a_task_intrinsic() {
    let (_, analysis) = analyze_source(
        r"
async fn child() Int { 1 }

async fn useTask[Task]() {
    discard Task.any(child()).await
}
",
    );
    assert!(analysis.has_errors());
    assert!(task_intrinsics(&analysis).is_empty());
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
fn application_cannot_extend_builtin_task_or_replace_source_sleep() {
    let (_, analysis) = analyze_source(
        r"
impl Task[Unit] {
    pub static method sleep(milliseconds Int) Task[Unit] {
        Task.sleep(milliseconds)
    }
}

fn misuse(task Task[Unit]) {
    discard task.sleep(1)
}
",
    );
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ForeignInherentImpl")
    );
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "AmbiguousInherentMethod")
    );
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "UnknownName")
    );
    assert!(task_intrinsics(&analysis).is_empty());
}

#[test]
fn task_sleep_leaf_is_private_to_the_unique_source_wrapper() {
    let task = parse_with_file(
        FileId(0),
        include_str!("../../../library/std/task/task.loom"),
    );
    let sibling = parse_with_file(
        FileId(1),
        r"
import std.task.__sleep

pub fn steal(milliseconds Int) Task[Unit] {
    __sleep(milliseconds)
}
",
    );
    assert!(task.diagnostics().is_empty());
    assert!(sibling.diagnostics().is_empty());
    let std = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: FileId(0),
            package: std.clone(),
            module: ModuleName::new("std.task"),
            syntax: task.ast(),
        },
        PackageSourceUnit {
            file: FileId(1),
            package: std.clone(),
            module: ModuleName::new("std.task"),
            syntax: sibling.ast(),
        },
    ]);
    lowered.program.register_package(std, [], true);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    let analysis = analyze(&lowered.program);
    assert!(analysis.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "UnknownName" && diagnostic.primary.file == FileId(1)
    }));
    let private_leaf_calls = analysis
        .typed
        .bodies
        .values()
        .flat_map(|body| body.calls.values())
        .filter(|call| call.target == CallTarget::Builtin(BuiltinValue::TaskSleep))
        .count();
    assert_eq!(private_leaf_calls, 1);
}

#[test]
fn missing_task_source_has_no_sleep_fallback() {
    let parsed = parse_with_file(
        FileId(0),
        r"
async fn main() {
    Task.sleep(0).await
}
",
    );
    assert!(parsed.diagnostics().is_empty());
    let lowered = lower_files([SourceUnit {
        file: FileId(0),
        syntax: parsed.ast(),
    }]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    let analysis = analyze(&lowered.program);
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "UnknownName")
    );
    assert!(task_intrinsics(&analysis).is_empty());
    assert!(analysis.typed.bodies.values().all(|body| {
        body.calls
            .values()
            .all(|call| call.target != CallTarget::Builtin(BuiltinValue::TaskSleep))
    }));
}
