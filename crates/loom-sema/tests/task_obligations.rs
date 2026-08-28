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

fn codes(diagnostics: &[loom_core::Diagnostic]) -> Vec<&str> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

#[test]
fn unused_direct_and_wrapped_task_locals_are_rejected() {
    let diagnostics = analyze_source(
        r"
module task_locals

record TaskBox {
    task Task[Int]
    label Int
}

async fn work() Int { 42 }

fn direct() {
    let pending = work()
}

fn wrapped() {
    let boxed = TaskBox { task = work(), label = 1 }
}

fn ordinaryFieldReadDoesNotConsume() {
    let boxed = TaskBox { task = work(), label = 1 }
    discard boxed.label
}
",
    );
    assert_eq!(
        codes(&diagnostics),
        [
            "UnawaitedAsyncCall",
            "UnawaitedAsyncCall",
            "UnawaitedAsyncCall"
        ],
        "{diagnostics:#?}"
    );
}

#[test]
fn task_obligations_are_recursive_for_parameters() {
    let diagnostics = analyze_source(
        r"
module task_params

enum Failure { Failed }

record TaskBox {
    task Task[Int]
}

fn ignored(
    direct Task[Int],
    tuple (Task[Int], Int),
    list List[Task[Int]],
    option Option[Task[Int]],
    outcome Result[Task[Int], Failure],
    boxed TaskBox
) {
}
",
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "UnawaitedAsyncCall")
            .count(),
        6,
        "{diagnostics:#?}"
    );
}

#[test]
fn await_tuple_join_and_sync_forward_consume_tasks() {
    let diagnostics = analyze_source(
        r"
module task_consumption

record TaskBox {
    task Task[Int]
}

async fn work() Int { 42 }

impl TaskBox {
    method forward(self) TaskBox {
        self
    }
}

fn forwardBox(value TaskBox) TaskBox {
    value.forward()
}

fn forward(value Task[Int]) Task[Int] {
    value
}

fn select(value Option[Task[Int]]) Task[Int] {
    match value {
        Some(task) => task
        None => work()
    }
}

async fn consume() {
    let direct = work()
    discard direct.await

    let left = work()
    let right = work()
    discard (left, right).await

    var tasks = List[Task[Int]]()
    tasks.add(work())
    discard Task.all(tasks).await

    discard forward(work()).await
    discard select(Some(work())).await

    let fromBlock = {
        let inner = work()
        inner
    }
    discard fromBlock.await
}
",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn repeated_and_conditional_consumption_are_tracked() {
    let diagnostics = analyze_source(
        r"
module task_flow

async fn work() Int { 42 }

async fn repeated() {
    let pending = work()
    discard pending.await
    discard pending.await
}

async fn conditional(flag Bool) {
    let pending = work()
    if flag {
        discard pending.await
    } else {
        Unit
    }
}

async fn both(flag Bool) {
    let pending = work()
    if flag {
        discard pending.await
    } else {
        discard pending.await
    }
}

async fn loopIsNotGuaranteed() {
    let pending = work()
    for index in 0..1 {
        discard pending.await
    }
}

async fn shortCircuit(flag Bool) {
    let pending = work()
    let combined = flag && pending.await == 42
    discard combined
}

async fn conditionThenConsume(flag Bool) {
    let pending = work()
    if flag {
        discard pending.await
    } else {
        Unit
    }
    discard pending.await
}

async fn reusedList() {
    var tasks = List[Task[Int]]()
    tasks.add(work())
    discard Task.all(tasks).await
    tasks.add(work())
}
",
    );
    let diagnostics = codes(&diagnostics);
    assert_eq!(
        diagnostics
            .iter()
            .filter(|code| **code == "TaskAlreadyConsumed")
            .count(),
        2,
        "{diagnostics:#?}"
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|code| **code == "TaskConditionallyConsumed")
            .count(),
        1,
        "{diagnostics:#?}"
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|code| **code == "UnawaitedAsyncCall")
            .count(),
        3,
        "{diagnostics:#?}"
    );
}

#[test]
fn propagation_audits_tasks_on_its_implicit_error_exit() {
    let diagnostics = analyze_source(
        r"
module task_propagation

enum Failure { Failed }

async fn work() Int { 42 }

fn fail() Result[Int, Failure] {
    Err(Failure.Failed)
}

async fn propagate() Result[Unit, Failure] {
    let pending = work()
    let value = fail()?
    discard value
    discard pending.await
    Ok(Unit)
}
",
    );
    assert_eq!(codes(&diagnostics), ["UnawaitedAsyncCall"]);
}

#[test]
fn propagation_transfers_a_task_carrying_result_once() {
    let diagnostics = analyze_source(
        r"
module task_propagation_transfer

enum Failure { Failed }

async fn consume(value Result[Task[Int], Failure]) Result[Unit, Failure] {
    let task = value?
    discard task.await
    Ok(Unit)
}
",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn early_return_audits_live_tasks_on_the_exiting_path() {
    let diagnostics = analyze_source(
        r"
module task_return

async fn work() Int { 42 }

async fn early(flag Bool) {
    let pending = work()
    if flag {
        return
    }
    discard pending.await
}
",
    );
    assert_eq!(codes(&diagnostics), ["UnawaitedAsyncCall"]);
}

#[test]
fn generic_and_async_task_transfer_boundaries_fail_closed() {
    let diagnostics = analyze_source(
        r"
module task_boundaries

async fn work() Int { 42 }

fn sink[T](value T) {}

async fn asyncSink(value Task[Int]) {
    discard value.await
}

async fn nestedResult() Task[Int] {
    work()
}

async fn caller() {
    sink(work())
    discard asyncSink(work()).await
}
",
    );
    let diagnostics = codes(&diagnostics);
    assert!(
        diagnostics.contains(&"TaskGenericTransferUnsupported"),
        "{diagnostics:#?}"
    );
    assert!(
        diagnostics.contains(&"TaskAsyncTransferUnsupported"),
        "{diagnostics:#?}"
    );
    assert!(
        diagnostics.contains(&"TaskAsyncResultUnsupported"),
        "{diagnostics:#?}"
    );
}

#[test]
fn instantiated_generic_receivers_and_async_results_cannot_hide_tasks() {
    let diagnostics = analyze_source(
        r"
module task_generic_boundaries

record GenericBox[T] {
    value T
}

record ConceptBox[T] {
    value T
}

concept Forget {
    method forget(self)
}

impl[T] GenericBox[T] {
    method forget(self) {}
}

impl[T] Forget for ConceptBox[T] {
    method forget(self) {}
}

async fn work() Int { 42 }

async fn maybe[T]() Option[T] { None }

fn inherentReceiver() {
    let boxed = GenericBox { value = work() }
    boxed.forget()
}

fn conceptReceiver() {
    let boxed = ConceptBox { value = work() }
    boxed.forget()
}

fn qualifiedConceptReceiver() {
    let boxed = ConceptBox { value = work() }
    <ConceptBox[Task[Int]] as Forget>.forget(boxed)
}

async fn instantiatedAsyncResult() {
    discard maybe[Task[Int]]().await
}
",
    );
    let diagnostics = codes(&diagnostics);
    assert_eq!(
        diagnostics
            .iter()
            .filter(|code| **code == "TaskGenericTransferUnsupported")
            .count(),
        3,
        "{diagnostics:#?}"
    );
    assert!(
        diagnostics.contains(&"TaskAsyncResultUnsupported"),
        "{diagnostics:#?}"
    );
}

#[test]
fn concrete_conformance_may_explicitly_transfer_a_task_receiver() {
    let diagnostics = analyze_source(
        r"
module task_concrete_receiver

record TaskBox {
    value Task[Int]
}

concept Forward {
    method forward(self) Self
}

impl Forward for TaskBox {
    method forward(self) TaskBox { self }
}

fn dot(value TaskBox) TaskBox {
    value.forward()
}

fn qualified(value TaskBox) TaskBox {
    <TaskBox as Forward>.forward(value)
}
",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn match_transfers_whole_carrier_and_checks_payloads() {
    let diagnostics = analyze_source(
        r"
module task_patterns

async fn consume(value Option[Task[Int]]) {
    match value {
        Some(task) => {
            discard task.await
        }
        None => Unit
    }
}

fn wildcard(value Option[Task[Int]]) {
    match value {
        Some(_) => Unit
        None => Unit
    }
}

fn unusedBinding(value Option[Task[Int]]) {
    match value {
        Some(task) => Unit
        None => Unit
    }
}
",
    );
    let diagnostics = codes(&diagnostics);
    assert_eq!(
        diagnostics
            .iter()
            .filter(|code| **code == "TaskPatternDiscard")
            .count(),
        1,
        "{diagnostics:#?}"
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|code| **code == "UnawaitedAsyncCall")
            .count(),
        1,
        "{diagnostics:#?}"
    );
}

#[test]
fn partial_projection_assignment_and_container_extraction_fail_closed() {
    let diagnostics = analyze_source(
        r#"
module task_partial

record Pair {
    left Task[Int]
    right Task[Int]
}

async fn work() Int { 42 }

async fn partial(value Pair) {
    discard value.left.await
}

fn overwrite() {
    var pending = work()
    pending = work()
}

fn listGet() {
    var tasks = List[Task[Int]]()
    tasks.add(work())
    let extracted = tasks.get(0)
}

fn mapGet() {
    let tasks = TextMap[Task[Int]]()
    let extracted = tasks.get("key")
}
"#,
    );
    let diagnostics = codes(&diagnostics);
    assert!(
        diagnostics.contains(&"TaskPartialExtractionUnsupported"),
        "{diagnostics:#?}"
    );
    assert!(
        diagnostics.contains(&"TaskAssignmentUnsupported"),
        "{diagnostics:#?}"
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|code| **code == "TaskContainerExtractionUnsupported")
            .count(),
        2,
        "{diagnostics:#?}"
    );
}

#[test]
fn discard_accepts_compound_expression_operands() {
    let diagnostics = analyze_source(
        r"
module discard_compound

fn compound(flag Bool, value Option[Int]) {
    discard { 1 }
    discard if flag { 1 } else { 2 }
    discard match value {
        Some(item) => item
        None => 0
    }
}
",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn discarding_a_scoped_resource_has_one_primary_diagnostic() {
    let diagnostics = analyze_source(
        r"
module standard.resource

concept Dispose {
    method dispose(mut self)
}

concept MustScope {}
concept NoSuspend {}

fn invalid(file File) {
    scoped resource = file
    discard resource
}
",
    );
    assert_eq!(codes(&diagnostics), ["MustScopeRequiresScoped"]);
}

#[test]
fn raw_handle_wait_constructors_are_not_language_builtins() {
    let diagnostics = analyze_source(
        r"
module raw_waits

async fn invalid() {
    Task.waitReadable(1).await
    Task.waitWritable(1).await
}
",
    );
    assert_eq!(
        codes(&diagnostics),
        ["UnknownName", "UnknownName", "UnknownName", "UnknownName"],
        "{diagnostics:#?}"
    );
}
