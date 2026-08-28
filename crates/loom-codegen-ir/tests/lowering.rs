use std::{collections::BTreeMap, fmt::Write as _, process::Command, time::Duration};

use loom_codegen_ir::{
    AwaitMode, CheckedIntBinaryOp, Effects, InstanceKey, InstructionKind, InvalidRootCode,
    LoweringErrorCode, LoweringOutcome, ResourceLimitCode, SourceArtifactRequest, TargetLayout,
    TerminatorKind, UnsupportedFeature, artifact_identity, dump_program, lower_typed_artifact,
};
use loom_core::{FileId, Span};
use loom_hir::{SourceUnit, lower_files};
use loom_lowering::lower_to_mir;
use loom_mir::{
    Block, CallPlan, Constant, ConstructionMode, Expr, ExprKind, FieldDef, Function, FunctionId,
    LocalDecl, LocalId, PreludeIds, Program, ScopedDisposal, Statement, StatementKind, Type,
    TypeDef, TypeDefKind, TypeId,
};
use loom_sema::analyze;
use loom_syntax::parse_with_file;
use wait_timeout::ChildExt as _;

fn compile(source: &str) -> loom_mir::CheckedProgram {
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
        .unwrap_or_else(|failure| panic!("MIR lowering diagnostics: {:#?}", failure.diagnostics()))
}

fn lower_run(source: &str) -> LoweringOutcome {
    let mir = compile(source);
    lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower typed artifact")
}

fn complete_dump(source: &str) -> String {
    let outcome = lower_run(source);
    let LoweringOutcome::Complete(artifact) = &outcome else {
        panic!("source should be completely supported: {outcome:?}")
    };
    dump_program(artifact.program())
}

#[test]
fn canonical_logging_builtins_lower_to_typed_fault_control_flow() {
    let outcome = lower_run(
        r#"module typed_logging

import standard.log.debug
import standard.log.info
import standard.log.warn
import standard.log.error
import standard.log.write

pub fn main() Unit {
    debug("debug")
    info("info")
    warn("warn")
    error("error")
    write(LogLevel.Warn, "event", TextMap[Text]())
    Unit
}
"#,
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("canonical logging source must lower completely: {outcome:?}")
    };
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main instance");
    assert_eq!(main.effects(), Effects::MAY_FAULT);

    let writes = main
        .blocks()
        .iter()
        .filter_map(
            |block| match block.terminator().map(loom_codegen_ir::Terminator::kind) {
                Some(TerminatorKind::LogWrite {
                    level,
                    message,
                    fields,
                    normal,
                    fault,
                }) => Some((*level, *message, *fields, normal, fault)),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 5, "{}", dump_program(artifact.program()));
    assert!(
        writes[..4]
            .iter()
            .all(|(_, _, fields, _, _)| fields.is_none())
    );
    assert!(writes[4].2.is_some());
    for (_, _, _, normal, fault) in &writes {
        assert_eq!(
            main.block(normal.block)
                .expect("log normal block")
                .params()
                .len(),
            1
        );
        assert!(matches!(
            main.block(fault.block)
                .and_then(loom_codegen_ir::Block::terminator)
                .map(loom_codegen_ir::Terminator::kind),
            Some(TerminatorKind::ResumeFault)
        ));
    }

    let dump = dump_program(artifact.program());
    assert_eq!(dump.matches("log.write ").count(), 5, "{dump}");
    assert_eq!(dump.matches("fields none").count(), 4, "{dump}");
    for variant in 0..=3 {
        assert!(
            dump.contains(&format!("sum.construct variant {variant} ()")),
            "{dump}"
        );
    }
}

const TYPED_ASYNC_SOURCE: &str = r"module lcir_typed_async

async fn echo(value Bool) Bool {
    value
}

pub async fn main() Unit {
    let observed = echo(true).await
    if observed {
        Unit
    } else {
        Unit
    }
}
";

const TYPED_TASK_ALL_SOURCE: &str = include_str!("../../../fixtures/lcir-typed-task-all/main.loom");

#[test]
fn async_scalar_call_and_await_lower_to_a_checked_coroutine_plan() {
    let outcome = lower_run(TYPED_ASYNC_SOURCE);
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("direct scalar async fixture must lower through typed LCIR")
    };
    let echo = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("echo"))
        .expect("echo instance");
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main instance");
    assert_eq!(echo.coroutine().expect("echo coroutine").suspensions(), []);
    let main_plan = main.coroutine().expect("main coroutine");
    assert_eq!(main_plan.suspensions().len(), 1);
    assert_eq!(main_plan.suspensions()[0].state(), 1);
    assert!(main_plan.suspensions()[0].live().is_empty());
    let dump = dump_program(artifact.program());
    let encoded_plan = format!(
        "coroutine output={} states=[1 all awaited=({}) live=()]",
        main_plan.output(),
        main_plan.suspensions()[0].awaited()[0]
    );
    assert!(dump.contains(&encoded_plan), "{dump}");
    assert!(artifact_identity(&artifact).contains(&encoded_plan));
    assert!(main.instructions().iter().any(|instruction| matches!(
        instruction.kind(),
        InstructionKind::TaskCreate { coroutine, .. } if coroutine == &echo.id()
    )));
    assert!(main.blocks().iter().any(|block| matches!(
        block.terminator().map(loom_codegen_ir::Terminator::kind),
        Some(TerminatorKind::AwaitTasks { state: 1, .. })
    )));
    let task = artifact
        .representations()
        .type_id(&Type::Task(Box::new(Type::Bool)))
        .expect("Task[Bool] type");
    assert_eq!(
        artifact
            .representations()
            .value_type(task)
            .and_then(|task| artifact.representations().repr(task.repr())),
        Some(&loom_codegen_ir::Repr::TaskHandle)
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the source fixture and its three explicit cleanup continuations stay visible together"
)]
fn async_scoped_and_defer_cleanup_are_explicit_on_every_suspension_exit() {
    let outcome = lower_run(
        r"module standard.resource

concept Dispose {
    method dispose(mut self) Unit
}

concept MustScope {}
concept NoSuspend {}

record Resource {
    value Int
}

impl Dispose for Resource {
    method dispose(mut self) Unit {
        assert self.value > 0
        self.value = 0
        Unit
    }
}

impl MustScope for Resource {}

async fn child() Int {
    Task.sleep(1).await
    7
}

pub async fn main() Unit {
    var marker = 0
    defer {
        marker = marker + 1
    }
    scoped resource = Resource { value = 3 }
    let value = child().await
    discard value
    Unit
}
",
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("scoped cleanup across suspension must lower through typed LCIR")
    };
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main instance");
    let await_edges = main
        .blocks()
        .iter()
        .filter_map(
            |block| match block.terminator().map(loom_codegen_ir::Terminator::kind) {
                Some(TerminatorKind::AwaitTasks {
                    normal,
                    fault,
                    cancel,
                    ..
                }) => Some((normal, fault, cancel)),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(await_edges.len(), 1);
    let (normal, fault, cancel) = await_edges[0];
    assert_eq!(normal.arguments.as_ref(), fault.arguments.as_ref());
    assert_eq!(normal.arguments.as_ref(), cancel.arguments.as_ref());
    assert_eq!(
        normal.arguments.len(),
        2,
        "defer-captured state and the scoped Resource must remain live in the coroutine frame"
    );

    let dispose_calls = main
        .blocks()
        .iter()
        .filter_map(|block| block.terminator())
        .filter(|terminator| matches!(terminator.kind(), TerminatorKind::Invoke { .. }))
        .count();
    assert_eq!(
        dispose_calls, 3,
        "Dispose must be statically expanded on normal completion, child fault, and cancellation"
    );
    let deferred_adds = main
        .blocks()
        .iter()
        .filter_map(|block| block.terminator())
        .filter(|terminator| {
            matches!(
                terminator.kind(),
                TerminatorKind::CheckedIntBinary {
                    op: CheckedIntBinaryOp::Add,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        deferred_adds, 6,
        "defer must run on all three exits and after a later scoped Dispose action faults"
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("task.cancelled"), "{dump}");
    assert!(dump.contains("resume_fault"), "{dump}");
    assert!(
        dump.contains(", fault b") && dump.contains(", cancel b"),
        "{dump}"
    );
}

#[test]
fn static_heterogeneous_task_all_uses_direct_and_first_class_checked_shapes() {
    let outcome = lower_run(TYPED_TASK_ALL_SOURCE);
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("fixed heterogeneous Task.all must lower through typed LCIR")
    };
    let exercise = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("exerciseJoins"))
        .expect("exerciseJoins instance");

    let joins = exercise
        .instructions()
        .iter()
        .filter_map(|instruction| match instruction.kind() {
            InstructionKind::TaskJoinAll { tasks } => Some((instruction, tasks.as_ref())),
            _ => None,
        })
        .collect::<Vec<_>>();
    let expected = [
        (3, Type::Tuple(vec![Type::Text, Type::Int, Type::Unit])),
        (3, Type::Tuple(vec![Type::Text, Type::Int, Type::Unit])),
        (2, Type::Tuple(vec![Type::Int, Type::Bool])),
    ];
    assert_eq!(
        joins.len(),
        expected.len(),
        "every stored composite needs one first-class allocation:\n{}",
        dump_program(artifact.program())
    );
    for ((instruction, tasks), (expected_width, expected_output)) in joins.iter().zip(&expected) {
        assert_eq!(tasks.len(), *expected_width);
        assert!(
            tasks
                .windows(2)
                .all(|pair| pair[0].index() < pair[1].index()),
            "child Task values must preserve left-to-right source evaluation order"
        );
        let join_result = exercise
            .value(instruction.results()[0])
            .expect("join result");
        assert_eq!(
            artifact
                .representations()
                .value_type(join_result.ty())
                .map(loom_codegen_ir::ValueType::semantic),
            Some(&Type::Task(Box::new(expected_output.clone())))
        );
    }

    let await_widths = exercise
        .blocks()
        .iter()
        .filter_map(
            |block| match block.terminator().map(loom_codegen_ir::Terminator::kind) {
                Some(TerminatorKind::AwaitTasks { tasks, .. }) => Some(tasks.len()),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(await_widths, [2, 2, 1, 1, 1, 1, 1]);
    assert_eq!(
        exercise
            .coroutine()
            .expect("exercise coroutine")
            .suspensions()
            .iter()
            .map(|suspension| suspension.awaited().len())
            .collect::<Vec<_>>(),
        await_widths,
        "coroutine rows must encode every implicit heterogeneous result slot"
    );
    assert!(
        exercise.instructions().iter().any(|instruction| {
            matches!(
                instruction.kind(),
                InstructionKind::ProductConstruct { fields } if fields.len() == 1
            )
        }),
        "one-element Task.all must construct the canonical one-field tuple"
    );

    let dump = dump_program(artifact.program());
    assert!(dump.contains("task.join_all("), "{dump}");
    assert!(dump.contains("await_tasks all state 1, ("), "{dump}");
    assert!(dump.contains("awaited=("), "{dump}");
    assert!(artifact_identity(&artifact).contains("task.join_all("));
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one route-selection matrix pins fixed, generic, stored, dynamic, and unsupported join modes together"
)]
fn immediately_awaited_fixed_task_any_lowers_but_stored_and_dynamic_joins_fall_back() {
    let fixed = r"module fixed_task_any

async fn child(value Int) Int { value }

pub async fn main() Unit {
    discard Task.any(child(1), child(2)).await
}
";
    let LoweringOutcome::Complete(fixed) = lower_run(fixed) else {
        panic!("immediately awaited fixed homogeneous Task.any must lower through typed LCIR")
    };
    let main = fixed
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main instance");
    let any = main
        .blocks()
        .iter()
        .find_map(
            |block| match block.terminator().map(loom_codegen_ir::Terminator::kind) {
                Some(TerminatorKind::AwaitTasks {
                    mode: AwaitMode::Any,
                    tasks,
                    normal,
                    ..
                }) => Some((tasks, normal)),
                _ => None,
            },
        )
        .expect("typed Task.any await");
    assert_eq!(any.0.len(), 2);
    assert_eq!(
        main.block(any.1.block)
            .expect("normal block")
            .params()
            .len(),
        1,
        "Task.any injects only the winning result"
    );
    assert_eq!(
        main.coroutine().unwrap().suspensions()[0].mode(),
        AwaitMode::Any
    );
    assert!(dump_program(fixed.program()).contains("await_tasks any state 1"));

    let generic = r"module generic_task_any

async fn child[T](value T) T { value }

async fn choose[T](first T, second T) T {
    Task.any(child(first), child(second)).await
}

pub async fn main() Unit {
    discard choose(1, 2).await
}
";
    let LoweringOutcome::Complete(generic) = lower_run(generic) else {
        panic!("a concrete generic Task.any instance must compare substituted child outputs")
    };
    assert!(dump_program(generic.program()).contains("await_tasks any"));

    let stored = r"module stored_task_any

async fn child(value Int) Int { value }

pub async fn main() Unit {
    let pending = Task.any(child(1), child(2))
    discard pending.await
}
";
    let LoweringOutcome::Unsupported(stored) = lower_run(stored) else {
        panic!("stored Task.any must remain one whole-artifact fallback")
    };
    assert!(
        stored
            .items()
            .iter()
            .any(|item| { item.feature() == UnsupportedFeature::TaskOperation })
    );

    let dynamic = r"module dynamic_task_all

async fn child(value Int) Int { value }

pub async fn main() Unit {
    let tasks = [child(1), child(2)]
    discard Task.all(tasks).await
}
";
    let LoweringOutcome::Unsupported(dynamic) = lower_run(dynamic) else {
        panic!("dynamic List Task.all must remain one whole-artifact fallback")
    };
    assert!(
        dynamic
            .items()
            .iter()
            .any(|item| { item.feature() == UnsupportedFeature::TaskOperation })
    );

    let terminal = r#"module terminal_task_joins

async fn child(value Int) Int { value }
async fn label() Text { "two" }

pub async fn main() Unit {
    discard Task.settled(child(1), label()).await
    discard Task.race(child(1), child(2)).await
}
"#;
    let LoweringOutcome::Complete(terminal) = lower_run(terminal) else {
        panic!("immediately awaited fixed Task.settled/race must lower through typed LCIR")
    };
    let main = terminal
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main instance");
    assert_eq!(
        main.instructions()
            .iter()
            .filter(|instruction| {
                matches!(instruction.kind(), InstructionKind::TaskOutcomeTake { .. })
            })
            .count(),
        3,
        "settled takes every terminal child and race takes only its winner"
    );
    let dump = dump_program(terminal.program());
    assert!(dump.contains("await_tasks settled state 1"), "{dump}");
    assert!(dump.contains("await_tasks race state 2"), "{dump}");
}

#[test]
fn sole_nonempty_task_list_literals_expand_to_fixed_rows_without_input_list_values() {
    let source = r"module static_task_list_joins

async fn child(value Int) Int { value }

pub async fn main() Unit {
    discard Task.all([child(1), child(2)]).await
    discard Task.any([child(3), child(4)]).await
    discard Task.settled([child(5), child(6)]).await
    discard Task.race([child(7), child(8)]).await
}
";
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("all four sole nonempty Task List literals must lower as fixed rows")
    };
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main instance");
    let awaits = main
        .blocks()
        .iter()
        .filter_map(
            |block| match block.terminator().map(loom_codegen_ir::Terminator::kind) {
                Some(TerminatorKind::AwaitTasks {
                    mode,
                    tasks,
                    normal,
                    ..
                }) => {
                    let result_width = match mode {
                        AwaitMode::All | AwaitMode::Settled => tasks.len(),
                        AwaitMode::Any | AwaitMode::Race => 1,
                    };
                    let result_types = main
                        .block(normal.block)
                        .expect("await normal block")
                        .params()
                        .iter()
                        .take(result_width)
                        .map(|parameter| {
                            main.value(*parameter)
                                .and_then(|value| artifact.representations().value_type(value.ty()))
                                .map(|value| value.semantic().clone())
                                .expect("await normal parameter type")
                        })
                        .collect::<Vec<_>>();
                    Some((*mode, tasks.len(), result_types))
                }
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(
        awaits,
        [
            (AwaitMode::All, 2, vec![Type::Int, Type::Int]),
            (AwaitMode::Any, 2, vec![Type::Int]),
            (
                AwaitMode::Settled,
                2,
                vec![
                    Type::Task(Box::new(Type::Int)),
                    Type::Task(Box::new(Type::Int)),
                ],
            ),
            (AwaitMode::Race, 2, vec![Type::Task(Box::new(Type::Int))],),
        ]
    );
    assert_eq!(
        main.instructions()
            .iter()
            .filter(|instruction| {
                matches!(instruction.kind(), InstructionKind::ListConstruct { .. })
            })
            .count(),
        2,
        "only all/settled output Lists are materialized; no input List[Task] exists in LCIR"
    );
    assert_eq!(
        main.instructions()
            .iter()
            .filter(|instruction| {
                matches!(instruction.kind(), InstructionKind::TaskOutcomeTake { .. })
            })
            .count(),
        3
    );
    assert!(
        main.instructions().iter().all(|instruction| {
            !matches!(instruction.kind(), InstructionKind::TaskJoinAll { .. })
        })
    );
    assert!(
        main.coroutine()
            .expect("main coroutine")
            .suspensions()
            .iter()
            .all(|suspension| suspension.awaited().len() == 2)
    );
}

#[test]
fn empty_and_stored_task_list_joins_remain_whole_artifact_fallbacks() {
    let empty = r"module empty_task_list_join

pub async fn main() Unit {
    discard Task.all(List[Task[Int]]()).await
}
";
    let LoweringOutcome::Unsupported(empty) = lower_run(empty) else {
        panic!("an empty Task List must not masquerade as a fixed runtime-length join")
    };
    assert!(empty.items().iter().any(|item| {
        matches!(
            item.feature(),
            UnsupportedFeature::TaskOperation | UnsupportedFeature::Suspension
        )
    }));

    let stored = r"module stored_task_list_join

async fn child(value Int) Int { value }

pub async fn main() Unit {
    let pending = Task.all([child(1), child(2)])
    discard pending.await
}
";
    let LoweringOutcome::Unsupported(stored) = lower_run(stored) else {
        panic!("a stored List join must remain a first-class dynamic-List fallback")
    };
    assert!(
        stored
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::TaskOperation)
    );
}

#[test]
fn task_fault_accessors_lower_to_direct_product_extracts() {
    let source = r"module task_fault_accessors

async fn broken() Int {
    assert false
    0
}

async fn child() Int { 1 }

pub async fn main() Unit {
    let outcome = Task.race([broken(), child()]).await
    match outcome {
        Completed(_) => Unit
        Faulted(fault) => {
            discard fault.code()
            discard fault.message()
            Unit
        }
        Cancelled => Unit
    }
}
";
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("canonical TaskFault code/message accessors must lower through typed LCIR")
    };
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main instance");
    let fields = main
        .instructions()
        .iter()
        .filter_map(|instruction| match instruction.kind() {
            InstructionKind::ProductExtract { field, .. } => Some(*field),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(fields.contains(&0), "TaskFault.code must extract field 0");
    assert!(
        fields.contains(&1),
        "TaskFault.message must extract field 1"
    );
}

#[test]
fn core03_task_list_joins_are_complete_for_run_and_tests() {
    let program = compile(include_str!("../../../examples/core03/tasks.loom"));
    for (route, request) in [
        (
            "run",
            SourceArtifactRequest::Run {
                entry: "main".into(),
            },
        ),
        ("tests", SourceArtifactRequest::Tests),
    ] {
        let outcome = lower_typed_artifact(
            &program,
            &request,
            TargetLayout::new(64).expect("test target"),
        )
        .expect("lower Core03 tasks artifact");
        let LoweringOutcome::Complete(_) = outcome else {
            panic!("Core03 {route} must have zero LCIR fallback items: {outcome:?}")
        };
    }
}

#[test]
fn task_all_in_a_sync_helper_receives_the_transitive_executor_capability() {
    let source = r"module sync_task_all

async fn child(value Int) Int { value }

fn combined() Task[(Int, Int)] {
    Task.all(child(1), child(2))
}

pub async fn main() {
    discard combined().await
}
";
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("a sync Task.all helper must lower through typed LCIR")
    };
    let combined = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("combined"))
        .expect("combined instance");
    assert!(combined.coroutine().is_none());
    assert!(combined.effects().contains(Effects::NEEDS_EXECUTOR));
    assert!(
        combined
            .instructions()
            .iter()
            .any(|instruction| matches!(instruction.kind(), InstructionKind::TaskJoinAll { .. }))
    );
}

#[test]
fn task_sleep_normalizes_duration_and_preserves_first_class_task_flow() {
    let source = r"module lcir_typed_sleep

import standard.time.milliseconds

pub async fn main() Unit {
    let delay = milliseconds(0)
    let timer = Task.sleep(delay)
    let marker = 42
    timer.await
    assert marker == 42
    Unit
}
";
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("first-class typed timer Task must lower completely")
    };
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main instance");
    assert!(main.effects().contains(loom_codegen_ir::Effects::MAY_FAULT));
    assert!(
        main.effects()
            .contains(loom_codegen_ir::Effects::NEEDS_EXECUTOR)
    );
    assert_eq!(
        main.coroutine()
            .expect("main coroutine")
            .suspensions()
            .len(),
        1
    );
    assert!(main.instructions().iter().any(|instruction| matches!(
        instruction.kind(),
        InstructionKind::ProductExtract { field: 0, .. }
    )));
    assert!(main.blocks().iter().any(|block| matches!(
        block.terminator().map(loom_codegen_ir::Terminator::kind),
        Some(TerminatorKind::TaskSleep { .. })
    )));
    let dump = dump_program(artifact.program());
    let extract = dump.find("product.extract").expect("Duration extraction");
    let sleep = dump.find("task.sleep").expect("typed timer terminator");
    let await_task = dump.find("await_tasks").expect("later first-class await");
    assert!(extract < sleep && sleep < await_task, "{dump}");
    let identity = artifact_identity(&artifact);
    assert!(identity.contains("task.sleep"), "{identity}");
    assert!(
        dump.contains("effects=may_fault+needs_runtime+needs_executor+may_suspend"),
        "{dump}"
    );
}

#[test]
fn typed_task_handles_fail_closed_on_a_32_bit_target() {
    let mir = compile(TYPED_ASYNC_SOURCE);
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(32).expect("32-bit target layout"),
    )
    .expect("classify typed async on 32-bit target");
    assert!(
        matches!(outcome, LoweringOutcome::Unsupported(_)),
        "Task handles require the pinned 64-bit runtime ABI: {outcome:?}"
    );
}

#[test]
fn fallible_async_calls_contracts_and_pointer_free_results_lower_atomically() {
    let source = r"module fallible_async

enum Problem { Wrong }

fn checkedDivide(value Int, divisor Int) Int { value / divisor }

async fn outcome(value Int) Result[Int, Problem] { Ok(value) }

async fn checkedAnswer(value Int, divisor Int) Int
    requires divisor != 0
    ensures result == 42
{
    assert value == 84
    checkedDivide(value, divisor)
}

pub async fn main() Unit {
    let completed = outcome(7).await
    let answer = checkedAnswer(84, 2).await
    match completed {
        Ok(value) => {
            assert value == 7 && answer == 42
            Unit
        }
        Err(_) => {
            assert false
            Unit
        }
    }
    Unit
}
";
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("fallible pointer-free Result coroutine must lower through typed LCIR")
    };
    let checked_answer = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("checkedAnswer"))
        .expect("checkedAnswer instance");
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main instance");
    assert!(checked_answer.coroutine().is_some());
    assert!(
        checked_answer
            .effects()
            .contains(loom_codegen_ir::Effects::MAY_FAULT)
    );
    assert!(main.effects().contains(loom_codegen_ir::Effects::MAY_FAULT));
    assert_eq!(
        main.coroutine()
            .expect("main coroutine")
            .suspensions()
            .len(),
        2
    );
    let dump = dump_program(artifact.program());
    for required in [
        "task.create",
        "await_tasks",
        "invoke",
        "contract PreconditionFault",
        "contract PostconditionFault",
        "resume_fault",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
}

#[test]
fn async_exit_contract_parameters_cross_suspension_without_retaining_unused_inputs() {
    let source = r"module async_exit_contract_liveness

async fn constrained(ignored Int, required Int, oldRequired Int) Int
    ensures result >= required && old(oldRequired) == oldRequired
{
    Task.sleep(0).await
    7
}

pub async fn main() Unit {
    let observed = constrained(99, 3, 4).await
    assert observed == 7
    Unit
}
";
    let mir = compile(source);
    let constrained = mir
        .as_program()
        .functions
        .iter()
        .find(|function| function.name.ends_with("constrained"))
        .expect("contracted async MIR function");
    let live_names = constrained.suspension_points[0]
        .live_locals
        .iter()
        .map(|local| {
            constrained
                .params
                .iter()
                .chain(&constrained.locals)
                .find(|candidate| candidate.id == *local)
                .expect("live local declaration")
                .name
                .as_str()
        })
        .collect::<Vec<_>>();
    assert_eq!(live_names, ["required", "oldRequired"]);

    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower typed artifact") else {
        panic!("exit-contract-only async parameters must lower through typed LCIR")
    };
    let constrained = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("constrained"))
        .expect("contracted LCIR instance");
    assert_eq!(
        constrained
            .coroutine()
            .expect("contracted coroutine plan")
            .suspensions()[0]
            .live()
            .len(),
        2,
        "only the two contract-referenced inputs belong in the frame"
    );
}

#[test]
fn async_generic_contract_fixture_has_a_complete_contract_aware_lcir_test_route() {
    let mir = compile(include_str!(
        "../../../fixtures/async-generic-contracts/main.loom"
    ));
    let violates_postcondition = mir
        .as_program()
        .functions
        .iter()
        .find(|function| function.name.ends_with("violatesPostcondition"))
        .expect("postcondition-only parameter fixture");
    let live_names = violates_postcondition.suspension_points[0]
        .live_locals
        .iter()
        .map(|local| {
            violates_postcondition
                .params
                .iter()
                .chain(&violates_postcondition.locals)
                .find(|candidate| candidate.id == *local)
                .expect("live local declaration")
                .name
                .as_str()
        })
        .collect::<Vec<_>>();
    assert_eq!(live_names, ["value", "minimum"]);

    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower typed test artifact") else {
        panic!("async generic contract tests must have one complete LCIR route")
    };
    let violates_postcondition = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("violatesPostcondition"))
        .expect("postcondition LCIR instance");
    assert_eq!(
        violates_postcondition
            .coroutine()
            .expect("postcondition coroutine plan")
            .suspensions()[0]
            .live()
            .len(),
        2
    );
}

#[test]
fn async_root_preconditions_lower_into_the_checked_task_entry() {
    let source = r"module async_root_contract

pub async fn main() Unit
    requires false
    ensures false
{
    Unit
}
";
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("an async root precondition must lower into its Task entry")
    };
    let root = artifact.run_root().expect("run root");
    let root = artifact.function(root).expect("root function");
    assert!(
        root.coroutine()
            .expect("root coroutine")
            .carries_caller_span()
    );
    assert!(root.effects().contains(Effects::MAY_FAULT));
    let dump = dump_program(artifact.program());
    assert!(dump.contains("caller_span=carried"), "{dump}");
    assert!(
        dump.contains("contract PreconditionFault")
            && dump.contains("blame_span=coroutine_call_site"),
        "{dump}"
    );
    assert!(!dump.contains("checked-root source="), "{dump}");
}

#[test]
fn task_creation_does_not_inherit_child_body_collection_effects() {
    let source = r#"module async_effect_boundary

async fn child(value Int) Int
    requires value > 0
{
    discard "left".concat("right").length()
    value
}

pub async fn main() Unit {
    let value = child(1).await
    assert value == 1
    Unit
}
"#;
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("a contracted collecting child must lower through typed LCIR")
    };
    let child = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("child"))
        .expect("child coroutine");
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("main coroutine");

    assert!(child.effects().contains(Effects::MAY_COLLECT));
    assert!(child.effects().contains(Effects::MAY_FAULT));
    assert!(
        child
            .coroutine()
            .expect("child coroutine plan")
            .carries_caller_span()
    );
    assert!(!main.effects().contains(Effects::MAY_COLLECT));
    assert!(main.effects().contains(Effects::MAY_FAULT));
    assert!(main.effects().contains(Effects::MAY_SUSPEND));
}

#[test]
fn managed_sum_coroutine_frames_lower_with_exact_direct_types() {
    let source = r#"module managed_result_async

enum Problem { Wrong }

async fn child() Result[Text, Problem] { Ok("man".concat("aged")) }

pub async fn main() Unit {
    match child().await {
        Ok(text) => {
            discard text.length()
            Unit
        }
        Err(_) => Unit
    }
    Unit
}
"#;
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("collision-free managed sum coroutine frame must lower atomically")
    };
    let child = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("child"))
        .expect("managed Result child instance");
    let plan = child.coroutine().expect("managed Result coroutine plan");
    assert_eq!(plan.output(), child.signature().result());
    let dump = dump_program(artifact.program());
    assert!(dump.contains("managed_ptr"), "{dump}");
    assert!(dump.contains("sum"), "{dump}");
}

#[test]
fn async_local_inout_calls_reuse_synchronous_functional_writeback() {
    let source = r"module async_local_inout

record Counter { value Int }

impl Counter {
    method update(mut self) Unit {
        self.value = 42
        Unit
    }
}

pub async fn main() Unit {
    var counter = Counter { value = 0 }
    counter.update()
    Unit
}
";
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("a synchronous inout call inside a coroutine must use typed LCIR")
    };
    let update = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("update"))
        .expect("synchronous update instance");
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("async main instance");
    assert_eq!(update.signature().inout_params(), [0]);
    assert!(main.signature().inout_params().is_empty());
    assert!(main.coroutine().is_some());
    assert!(main.instructions().iter().any(|instruction| matches!(
        instruction.kind(),
        InstructionKind::DirectCall { callee, .. }
            if callee == &update.id() && instruction.results().len() == 2
    )));
}

#[test]
fn async_mutable_view_parameters_are_independent_task_frame_values() {
    let source = r"module async_owned_view

dyn concept Source {
    method next(mut self) Int
}

record Counter { value Int }

impl Source for Counter {
    method next(mut self) Int {
        self.value = 1
        self.value
    }
}

async fn takeOwned(source Source) Int {
    source.next()
}

pub async fn main() Unit {
    let original = Counter { value = 0 }
    let observed = takeOwned(original).await
    assert observed == 1
    assert original.value == 0
    Unit
}
";
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("a unique mutable View must enter an async frame by value")
    };
    let next = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("next"))
        .expect("synchronous dynamic requirement implementation");
    let take_owned = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("takeOwned"))
        .expect("owned-View coroutine");
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("async main instance");

    assert_eq!(next.signature().inout_params(), [0]);
    assert!(take_owned.signature().inout_params().is_empty());
    assert!(take_owned.coroutine().is_some());
    assert_eq!(
        take_owned.signature().params(),
        &next.signature().params()[..1]
    );
    assert!(take_owned.instructions().iter().any(|instruction| matches!(
        instruction.kind(),
        InstructionKind::DirectCall { callee, .. }
            if callee == &next.id() && instruction.results().len() == 2
    )));
    assert!(take_owned.blocks().iter().all(|block| {
        block
            .terminator()
            .is_none_or(|terminator| terminator.writebacks().is_empty())
    }));
    assert!(main.instructions().iter().any(|instruction| matches!(
        instruction.kind(),
        InstructionKind::TaskCreate {
            coroutine,
            arguments,
        } if coroutine == &take_owned.id()
            && arguments.len() == 1
            && instruction.results().len() == 1
    )));
}

#[test]
fn async_nested_unique_views_are_physicalized_at_every_frame_node() {
    let source = r"module async_nested_owned_view

dyn concept Source {
    method next(mut self) Int
}

record Counter { value Int }
record Envelope { source dyn Source }

impl Source for Counter {
    method next(mut self) Int {
        self.value = 7
        self.value
    }
}

async fn takeNested(envelope Envelope) Int {
    var source = envelope.source
    source.next()
}

pub async fn main() Unit {
    let original = Counter { value = 0 }
    let observed = takeNested(Envelope { source = original }).await
    assert observed == 7
    assert original.value == 0
    Unit
}
";
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("a nested unique View must physicalize throughout the coroutine frame")
    };
    let take_nested = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("takeNested"))
        .expect("nested owned-View coroutine");
    assert!(take_nested.coroutine().is_some());
    assert!(take_nested.signature().inout_params().is_empty());
    assert!(
        take_nested
            .instructions()
            .iter()
            .any(|instruction| matches!(
                instruction.kind(),
                InstructionKind::DirectCall { .. } if instruction.results().len() == 2
            ))
    );
}

#[test]
fn async_finite_and_open_views_remain_atomic_signature_fallback() {
    let finite = r"module async_finite_view

dyn concept Source {
    method next(mut self) Int
}

record First { value Int }
record Second { value Int }

impl Source for First {
    method next(mut self) Int { self.value }
}

impl Source for Second {
    method next(mut self) Int { self.value }
}

async fn takeOwned(source Source) Int {
    source.next()
}

pub async fn main() Unit {
    let first = takeOwned(First { value = 1 }).await
    let second = takeOwned(Second { value = 2 }).await
    assert first == 1
    assert second == 2
    Unit
}
";
    let open = r"module async_open_view

dyn concept Source {
    method next(mut self) Int
}

record Boxed[T] { value T }

impl[T] Source for Boxed[T] {
    method next(mut self) Int { 1 }
}

async fn takeOwned(source Source) Int {
    source.next()
}

pub async fn main() Unit {
    let observed = takeOwned(Boxed { value = 1 }).await
    assert observed == 1
    Unit
}
";
    let nested_finite = r"module async_nested_finite_view

dyn concept Source {
    method next(mut self) Int
}

record First { value Int }
record Second { value Int }
record Envelope { source dyn Source }

impl Source for First {
    method next(mut self) Int { self.value }
}

impl Source for Second {
    method next(mut self) Int { self.value }
}

async fn takeNested(envelope Envelope) Int {
    var source = envelope.source
    source.next()
}

pub async fn main() Unit {
    let first = takeNested(Envelope { source = First { value = 1 } }).await
    let second = takeNested(Envelope { source = Second { value = 2 } }).await
    assert first == 1
    assert second == 2
    Unit
}
";
    for (label, source) in [
        ("finite", finite),
        ("open", open),
        ("nested finite", nested_finite),
    ] {
        let LoweringOutcome::Unsupported(report) = lower_run(source) else {
            panic!("{label} dynamic coroutine frame must remain atomic fallback")
        };
        assert!(
            report
                .items()
                .iter()
                .any(|item| item.feature() == UnsupportedFeature::SignatureType),
            "{label}: {report:#?}"
        );
    }
}

#[test]
fn async_unique_views_may_contain_managed_lists() {
    let source = r"module async_unique_view_with_list

dyn concept Source {
    method next(mut self) Int
}

record Boxed { values List[Int] }

impl Source for Boxed {
    method next(mut self) Int { self.values.length() }
}

async fn takeOwned(source Source) Int {
    source.next()
}

pub async fn main() Unit {
    let observed = takeOwned(Boxed { values = [1] }).await
    assert observed == 1
    Unit
}
";

    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("a managed List nested under a uniquely erased View must use typed LCIR")
    };
    let take_owned = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("takeOwned"))
        .expect("owned-View coroutine");
    assert!(take_owned.coroutine().is_some());
    assert!(take_owned.signature().inout_params().is_empty());
    assert!(take_owned.signature().params().iter().any(|ty| {
        let Some(value) = artifact.representations().value_type(*ty) else {
            return false;
        };
        let Some(loom_codegen_ir::Repr::Product(product)) =
            artifact.representations().repr(value.repr())
        else {
            return false;
        };
        artifact
            .representations()
            .product(*product)
            .is_some_and(|product| {
                product.fields().iter().any(|field| {
                    artifact
                        .representations()
                        .value_type(*field)
                        .and_then(|field| artifact.representations().repr(field.repr()))
                        == Some(&loom_codegen_ir::Repr::ManagedPointer)
                })
            })
    }));
}

#[test]
fn async_fault_cleanup_reads_the_synchronous_callee_writeback() {
    let source = r"module async_fault_writeback

record Counter { value Int }

impl Counter {
    method updateThenFail(mut self) Unit {
        self.value = 42
        assert false
        Unit
    }
}

pub async fn main() Unit {
    var counter = Counter { value = 0 }
    defer {
        let cleaned = counter.value
        assert cleaned == 42
    }
    Task.sleep(0).await
    counter.updateThenFail()
    Unit
}
";
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("fault-edge inout writeback must compose with async cleanup")
    };
    let update = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("updateThenFail"))
        .expect("fallible synchronous update instance");
    let main = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("main"))
        .expect("async main instance");
    assert_eq!(update.signature().inout_params(), [0]);
    assert!(update.effects().contains(Effects::MAY_FAULT));
    assert!(main.effects().contains(Effects::MAY_FAULT));
    assert!(main.effects().contains(Effects::MAY_SUSPEND));
    assert!(main.signature().inout_params().is_empty());

    let unwind = main
        .blocks()
        .iter()
        .find_map(
            |block| match block.terminator().map(loom_codegen_ir::Terminator::kind) {
                Some(TerminatorKind::Invoke { callee, unwind, .. }) if callee == &update.id() => {
                    Some(unwind.block)
                }
                _ => None,
            },
        )
        .expect("fallible inout invoke");
    let writeback_bridge = main.block(unwind).expect("writeback bridge");
    let [writeback] = writeback_bridge.params() else {
        panic!("fault edge must receive exactly the mutable receiver writeback")
    };
    let cleanup = match writeback_bridge
        .terminator()
        .map(loom_codegen_ir::Terminator::kind)
    {
        Some(TerminatorKind::Jump(target)) => target.block,
        terminator => panic!("writeback bridge must enter cleanup: {terminator:?}"),
    };
    let cleanup = main.block(cleanup).expect("fault cleanup block");
    assert!(cleanup.instructions().iter().any(|instruction| {
        main.instruction(*instruction).is_some_and(|instruction| {
            matches!(
                instruction.kind(),
                InstructionKind::ProductExtract { aggregate, field: 0 }
                    if aggregate == writeback
            )
        })
    }));
    assert!(matches!(
        cleanup.terminator().map(loom_codegen_ir::Terminator::kind),
        Some(TerminatorKind::Assert { .. })
    ));
}

#[test]
fn task_creation_in_sync_functions_propagates_the_executor_capability() {
    for (source, expected_task_create, expected_sleep) in [
        (
            r"module sync_task_create

async fn child() Int { 1 }

fn helper() Task[Int] { child() }

pub async fn main() {
    discard helper().await
}
",
            true,
            false,
        ),
        (
            r"module nested_sync_task_create

async fn child() Int { 1 }

fn inner() Task[Int] { child() }

fn helper() Task[Int] { inner() }

pub async fn main() {
    discard helper().await
}
",
            true,
            false,
        ),
        (
            r"module sync_task_sleep

fn helper() Task[Unit] { Task.sleep(0) }

pub async fn main() {
    helper().await
}
",
            false,
            true,
        ),
    ] {
        let LoweringOutcome::Complete(artifact) = lower_run(source) else {
            panic!("sync Task-producing helpers must lower through typed LCIR")
        };
        let synchronous_helpers = artifact
            .functions()
            .iter()
            .filter(|function| {
                function.coroutine().is_none()
                    && (function.name().ends_with("helper") || function.name().ends_with("inner"))
            })
            .collect::<Vec<_>>();
        assert!(!synchronous_helpers.is_empty());
        assert!(
            synchronous_helpers
                .iter()
                .all(|function| function.effects().contains(Effects::NEEDS_EXECUTOR))
        );
        assert_eq!(
            synchronous_helpers.iter().any(|function| {
                function.instructions().iter().any(|instruction| {
                    matches!(instruction.kind(), InstructionKind::TaskCreate { .. })
                })
            }),
            expected_task_create
        );
        assert_eq!(
            synchronous_helpers.iter().any(|function| {
                function.blocks().iter().any(|block| {
                    matches!(
                        block.terminator().map(loom_codegen_ir::Terminator::kind),
                        Some(TerminatorKind::TaskSleep { .. })
                    )
                })
            }),
            expected_sleep
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the hand-built canonical File fixture keeps MIR identity and cleanup-edge evidence local"
)]
fn builtin_scoped_file_cleanup_lowers_to_one_typed_runtime_edge() {
    let span = Span::default();
    let file_id = TypeId(9);
    let file = Type::Nominal(file_id, Vec::new());
    let mut types = (0_u32..9)
        .map(|id| TypeDef {
            id: TypeId(id),
            name: format!("Placeholder{id}"),
            span,
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: Vec::new(),
                invariant: None,
            },
        })
        .collect::<Vec<_>>();
    types.push(TypeDef {
        id: file_id,
        name: "File".into(),
        span,
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "raw".into(),
                ty: Type::Int,
                span,
            }],
            invariant: None,
        },
    });
    let mut program = Program {
        exports: BTreeMap::from([("main".into(), FunctionId(0))]),
        types,
        functions: vec![Function {
            id: FunctionId(0),
            name: "manual.main".into(),
            span,
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: vec![LocalDecl {
                id: LocalId(0),
                name: "file".into(),
                ty: file.clone(),
                mutable: true,
                span,
            }],
            return_ty: Type::Unit,
            receiver: None,
            body: Block {
                statements: vec![Statement {
                    kind: StatementKind::Scoped {
                        local: LocalId(0),
                        value: Expr::new(
                            ExprKind::Record {
                                ty: file_id,
                                type_arguments: Vec::new(),
                                fields: vec![Expr::new(
                                    ExprKind::Constant(Constant::Int(-1)),
                                    Type::Int,
                                    span,
                                )],
                                construction: ConstructionMode::Plain,
                            },
                            file,
                            span,
                        ),
                        disposal: ScopedDisposal::FileClose,
                    },
                    span,
                }],
                tail: Some(Box::new(Expr::new(
                    ExprKind::Constant(Constant::Unit),
                    Type::Unit,
                    span,
                ))),
                span,
            },
            call_plan: CallPlan::default(),
        }],
        prelude: PreludeIds {
            file: Some(file_id),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    program
        .renumber_expr_ids()
        .expect("number resource fixture");
    let program = program
        .into_checked()
        .expect("resource fixture is valid checked MIR");
    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower typed resource cleanup") else {
        panic!("canonical File cleanup must be in typed LCIR coverage")
    };
    let dump = dump_program(artifact.program());
    assert_eq!(dump.matches("resource.close.file").count(), 1, "{dump}");
    assert!(dump.contains("effects=may_fault+needs_runtime"), "{dump}");
    assert!(!dump.contains("loom.Value"), "{dump}");
}

#[test]
fn direct_cleanup_depth_has_a_stable_program_too_large_boundary() {
    let cleanups = "    defer { Unit }\n".repeat(1_025);
    let source =
        format!("module cleanup_budget\n\npub fn main() Unit {{\n{cleanups}    Unit\n}}\n");
    let mir = compile(&source);
    let error = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect_err("unbounded direct cleanup expansion must be rejected");
    assert_eq!(
        error.code(),
        LoweringErrorCode::ResourceLimit(ResourceLimitCode::ProgramTooLarge)
    );
    assert!(
        error
            .message()
            .contains("1024-action direct lexical cleanup depth"),
        "{error}"
    );
}

#[test]
fn literal_only_text_values_and_allocation_free_operations_lower_directly() {
    let dump = complete_dump(
        r#"module lcir_text

fn identity[T](value T) T { value }

fn inspect(value Text) Bool {
    value.length() == 6 && value.contains("界") && value == "hello界" && value != "other"
}

pub fn main() Unit {
    discard inspect(identity("hello界"))
    Unit
}
"#,
    );
    for expected in [
        "repr r5 = immortal_text_ptr",
        "type t5 = Text => r5",
        "types=[Text] witnesses=[]",
        "text.literal \"hello界\"",
        "text.length",
        "text.contains",
        "text.compare.equal",
        "text.compare.not_equal",
    ] {
        assert!(dump.contains(expected), "missing `{expected}`:\n{dump}");
    }
}

#[test]
fn text_literal_bytes_participate_in_artifact_identity() {
    let identity = |literal: &str| {
        let source = format!(
            "module lcir_text_identity\n\npub fn main() Unit {{\n    discard \"{literal}\".length()\n    Unit\n}}\n"
        );
        let LoweringOutcome::Complete(artifact) = lower_run(&source) else {
            panic!("bounded Text literal should lower directly")
        };
        artifact_identity(&artifact)
    };
    assert_ne!(identity("alpha"), identity("omega"));
}

#[test]
fn immortal_text_requires_the_pinned_64_bit_runtime_layout() {
    let mir = compile(
        r#"module lcir_text_32

pub fn main() Unit {
    discard "literal".length()
    Unit
}
"#,
    );
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(32).expect("32-bit test target"),
    )
    .expect("classify 32-bit Text artifact");
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("the 64-bit Text object ABI must not be guessed on a 32-bit target")
    };
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::TextConstant),
        "{report:?}"
    );
}

#[test]
fn text_product_without_a_literal_still_obeys_the_64_bit_pointer_boundary() {
    let mir = compile(
        r"module lcir_text_product_32

fn spin() (Text, Int) { spin() }

pub fn main() Unit {
    discard spin()
    Unit
}
",
    );
    let request = SourceArtifactRequest::Run {
        entry: "main".into(),
    };
    let outcome = lower_typed_artifact(
        &mir,
        &request,
        TargetLayout::new(32).expect("32-bit test target"),
    )
    .expect("classify a 32-bit Text-product artifact");
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("a Text-bearing product must not reach managed registration on a 32-bit target")
    };
    assert!(
        report.items().iter().any(|item| matches!(
            item.feature(),
            UnsupportedFeature::SignatureType | UnsupportedFeature::ExpressionType
        )),
        "{report:?}"
    );

    let outcome = lower_typed_artifact(
        &mir,
        &request,
        TargetLayout::new(64).expect("64-bit test target"),
    )
    .expect("classify a 64-bit Text-product artifact");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("the same Text-bearing product must remain direct on a 64-bit target")
    };
    let text = artifact
        .representations()
        .type_id(&loom_mir::Type::Text)
        .expect("Text type");
    assert_eq!(
        artifact
            .representations()
            .value_type(text)
            .and_then(|ty| artifact.representations().repr(ty.repr())),
        Some(&loom_codegen_ir::Repr::ManagedPointer)
    );
}

#[test]
fn text_sum_without_a_literal_still_obeys_the_64_bit_pointer_boundary() {
    let mir = compile(
        r"module lcir_text_sum_32

enum MaybeText {
    Missing
    Present(Text)
}

fn spin() MaybeText { spin() }

pub fn main() Unit {
    discard spin()
    Unit
}
",
    );
    let request = SourceArtifactRequest::Run {
        entry: "main".into(),
    };
    let outcome = lower_typed_artifact(
        &mir,
        &request,
        TargetLayout::new(32).expect("32-bit test target"),
    )
    .expect("classify a 32-bit Text-sum artifact");
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("a Text-bearing sum must fail closed before managed registration on 32-bit")
    };
    assert!(
        report.items().iter().any(|item| matches!(
            item.feature(),
            UnsupportedFeature::SignatureType | UnsupportedFeature::ExpressionType
        )),
        "{report:?}"
    );

    assert!(matches!(
        lower_typed_artifact(
            &mir,
            &request,
            TargetLayout::new(64).expect("64-bit test target"),
        )
        .expect("classify a 64-bit Text-sum artifact"),
        LoweringOutcome::Complete(_)
    ));
}

#[test]
fn text_selection_lowers_to_one_managed_collecting_instruction() {
    let outcome = lower_run(
        r#"module lcir_text_get

pub fn main() Unit {
    discard "a界🙂".get(1)
    discard "value".get(-1)
    Unit
}
"#,
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("Text selection must lower through complete typed LCIR")
    };
    let text = artifact
        .representations()
        .type_id(&loom_mir::Type::Text)
        .expect("Text type");
    assert_eq!(
        artifact
            .representations()
            .value_type(text)
            .and_then(|ty| artifact.representations().repr(ty.repr())),
        Some(&loom_codegen_ir::Repr::ManagedPointer)
    );
    assert!(artifact.functions().iter().all(|function| {
        function
            .effects()
            .contains(loom_codegen_ir::Effects::MAY_COLLECT)
    }));
    assert!(artifact.functions().iter().any(|function| {
        function.instructions().iter().any(|instruction| {
            matches!(
                instruction.kind(),
                InstructionKind::TextGet {
                    missing_variant: 0,
                    found_variant: 1,
                    ..
                }
            )
        })
    }));
}

#[test]
fn concat_selects_one_managed_text_representation_and_collection_effect() {
    let outcome = lower_run(
        r#"module lcir_text_concat

fn join(left Text, right Text) Text { left.concat(right) }

pub fn main() Unit {
    let joined = join("left", "right")
    discard joined.length()
    Unit
}
"#,
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("dynamic Text concat must lower through complete LCIR")
    };
    let text = artifact
        .representations()
        .type_id(&loom_mir::Type::Text)
        .expect("Text type");
    let representation = artifact
        .representations()
        .value_type(text)
        .and_then(|ty| artifact.representations().repr(ty.repr()));
    assert_eq!(representation, Some(&loom_codegen_ir::Repr::ManagedPointer));
    assert!(artifact.functions().iter().all(|function| {
        function
            .effects()
            .contains(loom_codegen_ir::Effects::MAY_COLLECT)
            && function
                .effects()
                .contains(loom_codegen_ir::Effects::NEEDS_RUNTIME)
            && !function
                .effects()
                .contains(loom_codegen_ir::Effects::MAY_FAULT)
    }));
    assert!(artifact.functions().iter().any(|function| {
        function
            .instructions()
            .iter()
            .any(|instruction| matches!(instruction.kind(), InstructionKind::TextConcat { .. }))
    }));
}

#[test]
fn text_product_selects_managed_provenance_without_inventing_runtime_effects() {
    let outcome = lower_run(
        r#"module lcir_text_product

record Named { value Text }

pub fn main() Unit {
    discard Named { value = "safe" }
    Unit
}
"#,
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("Text-bearing records must lower as direct managed products")
    };
    let text = artifact
        .representations()
        .type_id(&loom_mir::Type::Text)
        .expect("Text type");
    assert_eq!(
        artifact
            .representations()
            .value_type(text)
            .and_then(|ty| artifact.representations().repr(ty.repr())),
        Some(&loom_codegen_ir::Repr::ManagedPointer)
    );
    assert!(
        artifact
            .functions()
            .iter()
            .all(|function| function.effects().is_empty())
    );
    let dump = dump_program(artifact.program());
    assert!(dump.contains("managed_ptr"), "{dump}");
    assert!(dump.contains("product p0(t5)"), "{dump}");
}

#[test]
fn checked_mir_is_the_only_source_of_proven_refinement_instructions() {
    let dump = complete_dump(
        r"module proven_boundaries

type Money = Float where self >= 0.0

record Range {
    low Money
    high Money
    invariant self.low <= self.high
}

fn widen(value Money) Float { value }

pub fn main() Unit {
    let money = Money(10.0)
    let range = Range { low = Money(1.0), high = Money(2.0) }
    discard widen(money)
    discard range
    Unit
}
",
    );
    assert!(dump.contains("refine.proven"), "{dump}");
    assert!(dump.contains("unrefine"), "{dump}");
    assert!(dump.contains("invariant_record.proven"), "{dump}");
}

#[test]
fn portable_nongeneric_proof_rechecks_lower_to_typed_fault_guards() {
    let source = r"module portable_proof_fallback

type Money = Float where self >= 0.0

record Range {
    low Money
    high Money
    invariant self.low <= self.high
}

enum Stored {
    Amount(Money)
    Interval(Range)
}

pub fn main() Unit {
    let money = Money(10.0)
    discard Stored.Amount(money)
    discard Stored.Interval(Range { low = Money(1.0), high = Money(2.0) })
    Unit
}
";
    let fresh = compile(source);
    let fresh_outcome = lower_typed_artifact(
        &fresh,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower fresh process-local proofs");
    let LoweringOutcome::Complete(fresh_artifact) = fresh_outcome else {
        panic!("fresh process-local Proven constructions must use direct LCIR")
    };
    let fresh_dump = dump_program(fresh_artifact.program());
    assert!(fresh_dump.contains("refine.proven"), "{fresh_dump}");
    assert!(
        fresh_dump.contains("invariant_record.proven"),
        "{fresh_dump}"
    );
    assert!(fresh_dump.contains("sum.construct"), "{fresh_dump}");

    let bytes = loom_mir::encode_interpreted_executable_artifact(&fresh, "main")
        .expect("encode portable proof artifact");
    let (decoded, entry) = loom_mir::decode_interpreted_executable_artifact(&bytes)
        .expect("decode portable proof artifact");
    assert!(decoded.serialized_construction_proofs_were_distrusted());
    let decoded_debug = format!("{decoded:#?}");
    assert!(
        decoded_debug.contains("construction: Recheck"),
        "{decoded_debug}"
    );
    assert!(
        !decoded_debug.contains("construction: Proven"),
        "{decoded_debug}"
    );

    let decoded_outcome = lower_typed_artifact(
        &decoded,
        &SourceArtifactRequest::Run { entry },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("classify decoded proof replay");
    let LoweringOutcome::Complete(decoded_artifact) = decoded_outcome else {
        panic!("nongeneric proof rechecks must remain on the typed LCIR route")
    };
    let decoded_dump = dump_program(decoded_artifact.program());
    assert_eq!(
        decoded_dump
            .matches("runtime ArtifactProofRejected")
            .count(),
        4,
        "{decoded_dump}"
    );
    assert_eq!(
        decoded_dump.matches("refine.proven").count(),
        3,
        "{decoded_dump}"
    );
    assert!(
        decoded_dump.contains("invariant_record.proven"),
        "{decoded_dump}"
    );
}

#[test]
fn portable_generic_record_proof_rechecks_use_concrete_contract_types() {
    let source = r#"module portable_generic_proof

record Guarded[Label, Payload] {
    label Label
    payload Option[Payload]
    marker Float

    invariant self.marker >= 0.0 || match self.payload {
        Some(value) => true
        None => true
    }
}

fn wrap[T](value T) Guarded[Text, T] {
    Guarded { label = "typed", payload = Some(value), marker = 9.0 }
}

pub fn main() Unit {
    discard wrap(7)
    Unit
}
"#;
    let fresh = compile(source);
    let bytes = loom_mir::encode_interpreted_executable_artifact(&fresh, "main")
        .expect("encode portable generic proof artifact");
    let (decoded, entry) = loom_mir::decode_interpreted_executable_artifact(&bytes)
        .expect("decode portable generic proof artifact");
    assert!(decoded.serialized_construction_proofs_were_distrusted());
    let decoded_debug = format!("{decoded:#?}");
    assert!(
        decoded_debug.contains("construction: Recheck"),
        "{decoded_debug}"
    );

    let outcome = lower_typed_artifact(
        &decoded,
        &SourceArtifactRequest::Run { entry },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("classify decoded generic proof replay");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("generic proof rechecks must use typed LCIR: {outcome:?}")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.contains("[Text,Int]"), "{dump}");
    assert_eq!(
        dump.matches("runtime ArtifactProofRejected").count(),
        1,
        "{dump}"
    );
    assert_eq!(dump.matches("invariant_record.proven").count(), 1, "{dump}");
}

#[test]
fn generic_invariant_and_refined_record_instantiations_lower_directly() {
    let source = r"module generic_products

record Boxed[T] {
    value T
}

record Guarded[T] {
    value T
    marker Int
    invariant self.marker >= 0
}

type PositiveBox = Boxed[Int] where self.value >= 0

fn refined() PositiveBox {
    PositiveBox(Boxed { value = 1 })
}

fn established_invariant() Guarded[Int] {
    Guarded { value = 1, marker = 1 }
}

pub fn main() Unit {
    discard refined()
    discard established_invariant()
    Unit
}
";
    let program = compile(source);
    let mir_debug = format!("{program:#?}");
    assert!(
        mir_debug.contains("type_arguments: [\n") && mir_debug.contains("Int,"),
        "generic construction arguments must remain explicit in MIR: {mir_debug}"
    );
    let outcome = lower_typed_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower generic proof-bearing values");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("generic invariant/refined records must use typed LCIR")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.contains("Nominal#"), "{dump}");
    assert!(dump.contains("[Int]"), "{dump}");
    assert!(dump.contains("product.construct"), "{dump}");
    assert!(dump.contains("invariant_record.proven"), "{dump}");
    assert!(dump.contains("refine.proven"), "{dump}");
}

#[test]
fn empty_tests_are_one_complete_empty_artifact() {
    let mir = compile("module empty\n");
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower empty tests");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("empty test artifact should be complete")
    };
    assert!(artifact.functions().is_empty());
    assert_eq!(artifact.test_roots(), Some([].as_slice()));
}

#[test]
fn ordered_test_roots_form_one_complete_artifact() {
    let mir =
        compile("module tests\n\ntest fn first() { Unit }\n\ntest fn second() Unit { Unit }\n");
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower test artifact");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("scalar tests should be complete")
    };
    let roots = artifact.test_roots().expect("test roots");
    assert_eq!(roots.len(), 2);
    assert_eq!(
        roots
            .iter()
            .map(|root| artifact.function(*root).expect("root function").name())
            .collect::<Vec<_>>(),
        ["tests.first", "tests.second"]
    );
}

#[test]
fn source_lowering_routes_declarations_calls_and_roots_through_monomorphic_instance_keys() {
    let LoweringOutcome::Complete(artifact) = lower_run(
        r"module instance_regression

fn helper() Unit { Unit }

pub fn main() Unit { helper() }
",
    ) else {
        panic!("scalar source should lower completely")
    };
    let program = artifact.program().as_program();
    assert_eq!(program.instances().entries().len(), 2);
    for instance in program.instances().entries() {
        assert!(instance.key().is_monomorphic());
        assert_eq!(program.instance_key(instance.id()), Some(instance.key()));
        assert_eq!(
            program.instances().find(instance.key()),
            Some(instance.id())
        );
        assert_eq!(
            program
                .function(instance.id())
                .expect("planned function")
                .source(),
            instance.key().source()
        );
    }

    let root = artifact.run_root().expect("run root");
    let root_function = program.function(root).expect("root function");
    assert_eq!(
        program.instance_key(root),
        Some(&InstanceKey::monomorphic(root_function.source()))
    );
    let callees = program
        .functions()
        .iter()
        .flat_map(loom_codegen_ir::Function::instructions)
        .filter_map(|instruction| match instruction.kind() {
            InstructionKind::DirectCall { callee, .. } => Some(*callee),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(callees.len(), 1);
    let callee = callees[0];
    let callee_function = program.function(callee).expect("call target");
    assert_eq!(
        program.instance_key(callee),
        Some(&InstanceKey::monomorphic(callee_function.source()))
    );
}

#[test]
fn reachable_generic_calls_lower_to_exact_concrete_instances() {
    let outcome = lower_run(
        r"module generic_fallback

fn identity[T](value T) T { value }

pub fn main() Unit {
    discard identity(1)
    Unit
}
",
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("bounded generic source should lower completely")
    };
    let program = artifact.program().as_program();
    assert_eq!(program.instances().entries().len(), 2);
    let identity = program
        .instances()
        .entries()
        .iter()
        .find(|instance| !instance.key().is_monomorphic())
        .expect("generic identity instance");
    assert_eq!(identity.key().type_arguments(), &[loom_mir::Type::Int]);
    let dump = dump_program(artifact.program());
    assert!(
        dump.contains("instance i0 = source=f0 types=[Int] witnesses=[]"),
        "{dump}"
    );
    assert!(dump.contains("call i0"), "{dump}");
}

#[test]
fn generic_sum_construction_and_matches_plan_each_concrete_instance() {
    let dump = complete_dump(
        r"module generic_sums

fn unwrap[T](value Option[T], fallback T) T {
    match value {
        Some(found) => found
        None => fallback
    }
}

pub fn main() Unit {
    discard unwrap(Some(7), 0)
    discard unwrap(Some(true), false)
    Unit
}
",
    );

    assert_eq!(dump.matches("source=f0 types=[Int]").count(), 1, "{dump}");
    assert_eq!(dump.matches("source=f0 types=[Bool]").count(), 1, "{dump}");
    assert!(dump.contains("Nominal#0[Int]"), "{dump}");
    assert!(dump.contains("Nominal#0[Bool]"), "{dump}");
    assert!(dump.matches("sum.switch").count() >= 2, "{dump}");
}

#[test]
fn regular_generic_recursion_deduplicates_each_exact_instantiation() {
    let source = r"module generic_recursion

fn repeat[T](value T, remaining Int) T {
    if remaining == 0 {
        value
    } else {
        repeat(value, remaining - 1)
    }
}

pub fn main() Unit {
    discard repeat(7, 2)
    discard repeat(8, 1)
    discard repeat(true, 1)
    Unit
}
";
    let first = complete_dump(source);
    let second = complete_dump(source);
    assert_eq!(first, second);
    let lower = || {
        let mir = compile(source);
        let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
            &mir,
            &SourceArtifactRequest::Run {
                entry: "main".into(),
            },
            TargetLayout::new(64).expect("test target"),
        )
        .expect("lower reproducible generic artifact") else {
            panic!("regular generic recursion must be supported")
        };
        artifact
    };
    assert_eq!(artifact_identity(&lower()), artifact_identity(&lower()));
    assert_eq!(first.matches("source=f0 types=[Int]").count(), 1, "{first}");
    assert_eq!(
        first.matches("source=f0 types=[Bool]").count(),
        1,
        "{first}"
    );
    assert_eq!(first.matches("source=f1 types=[]").count(), 1, "{first}");
    assert!(first.contains("invoke i0"), "{first}");
    assert!(first.contains("invoke i1"), "{first}");
}

#[test]
fn erased_generic_proofs_remain_part_of_static_instance_identity() {
    let outcome = lower_run(
        r"module witnessed_instance

concept Marker {}
impl Marker for Int {}

fn preserve[T: Marker](value T) T { value }

pub fn main() Unit {
    discard preserve(7)
    Unit
}
",
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("a compile-time-only proof must not force legacy lowering")
    };
    let instance = artifact
        .program()
        .as_program()
        .instances()
        .entries()
        .iter()
        .find(|instance| !instance.key().witness_arguments().is_empty())
        .expect("witnessed generic instance");
    assert_eq!(instance.key().type_arguments(), &[loom_mir::Type::Int]);
    assert!(
        matches!(
            instance.key().witness_arguments(),
            [loom_codegen_ir::InstanceWitnessArgument::Concrete(_)]
        ),
        "{:?}",
        instance.key()
    );
}

#[test]
fn static_concept_calls_normalize_associated_types_and_conditional_proofs() {
    let source = r"module static_concepts

concept Truth {
    method truth(self) Bool
}

record Atom { value Bool }

impl Truth for Atom {
    method truth(self) Bool { self.value }
}

enum Wrapped[T] { Item(T) }

impl[T: Truth] Truth for Wrapped[T] {
    method truth(self) Bool {
        match self {
            Item(value) => value.truth()
        }
    }
}

fn evaluate[T: Truth](value T) Bool { value.truth() }

concept Source {
    associated type Item
    method first(self) Self.Item
}

record Number { value Int }

impl Source for Number {
    associated type Item = Int
    method first(self) Int { self.value }
}

fn read[T: Source](source T) T.Item { source.first() }

enum Problem { Failed }

fn verify() Result[Unit, Problem] {
    let truthful = evaluate(Wrapped.Item(Atom { value = true }))
    let number = read(Number { value = 42 })
    if truthful && number == 42 {
        Ok(Unit)
    } else {
        Err(Problem.Failed)
    }
}

pub fn main() Unit {
    match verify() {
        Ok(_) => Unit
        Err(_) => Unit
    }
}
";
    let first = lower_run(source);
    let LoweringOutcome::Complete(artifact) = first else {
        panic!("bounded concrete static dispatch must lower completely: {first:?}")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.contains("types=[Nominal#"), "{dump}");
    assert!(dump.contains("witnesses=[Apply#"), "{dump}");
    assert!(dump.contains("witnesses=[Concrete#"), "{dump}");
    assert!(!dump.contains("Projection#"), "{dump}");
    assert!(dump.matches("call i").count() >= 4, "{dump}");

    let second = lower_run(source);
    let LoweringOutcome::Complete(second) = second else {
        panic!("the same static artifact must remain complete")
    };
    assert_eq!(artifact_identity(&artifact), artifact_identity(&second));
}

#[test]
fn regular_static_dispatch_recursion_is_deduplicated_and_unused_witnesses_stay_dead() {
    let outcome = lower_run(
        r"module static_recursion

concept Step { method step(self, remaining Int) Int }
concept Unused { method unused(self) Int }

record Counter { value Int }

impl Step for Counter {
    method step(self, remaining Int) Int {
        if remaining == 0 {
            self.value
        } else {
            self.step(remaining - 1)
        }
    }
}

impl Unused for Counter {
    method unused(self) Int { 99 }
}

pub fn main() Unit {
    discard Counter { value = 7 }.step(3)
    Unit
}
",
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("regular static recursion must lower as one exact instance: {outcome:?}")
    };
    let step = artifact
        .functions()
        .iter()
        .find(|function| function.name().rsplit('.').next() == Some("step"))
        .expect("reachable witness method");
    assert!(step.blocks().iter().any(|block| matches!(
        block.terminator().map(loom_codegen_ir::Terminator::kind),
        Some(TerminatorKind::Invoke { callee, .. }) if *callee == step.id()
    )));
    assert!(
        artifact
            .functions()
            .iter()
            .all(|function| !function.name().ends_with(".unused")),
        "an unselected witness method must remain dead"
    );
}

#[test]
fn competing_closed_dynamic_witnesses_form_one_managed_catalog() {
    let LoweringOutcome::Complete(artifact) = lower_run(
        r"module dynamic_stays_erased

dyn concept Truth { method truth(self) Bool }
record Atom { value Bool }
record Other { value Bool }
impl Truth for Atom { method truth(self) Bool { self.value } }
impl Truth for Other { method truth(self) Bool { self.value } }

fn choose(first Bool) dyn Truth {
    if first {
        Atom { value = true }
    } else {
        Other { value = false }
    }
}

fn erased(value Truth) Bool { value.truth() }

pub fn main() Unit {
    discard erased(choose(true))
    Unit
}
",
    ) else {
        panic!("a finite closed dynamic witness set must lower as typed LCIR")
    };
    let representations = artifact.representations();
    let [dynamic] = representations.dynamics() else {
        panic!("one source view must produce one finite dynamic catalog")
    };
    assert_eq!(dynamic.candidates().len(), 2);
    assert_eq!(
        representations
            .value_type(dynamic.view())
            .and_then(|ty| representations.repr(ty.repr())),
        Some(&loom_codegen_ir::Repr::ManagedPointer)
    );
    assert!(artifact.functions().iter().any(|function| {
        function
            .instructions()
            .iter()
            .any(|instruction| matches!(instruction.kind(), InstructionKind::DynConstruct { .. }))
    }));
    assert!(artifact.functions().iter().any(|function| {
        function.blocks().iter().any(|block| {
            matches!(
                block.terminator().map(loom_codegen_ir::Terminator::kind),
                Some(TerminatorKind::DynSwitch { cases, .. }) if cases.len() == 2
            )
        })
    }));
    assert!(
        artifact.functions().iter().any(|function| {
            function
                .instructions()
                .iter()
                .any(|instruction| matches!(instruction.kind(), InstructionKind::DirectCall { .. }))
        }),
        "finite dynamic dispatch must end in direct candidate calls"
    );
}

#[test]
fn missing_dynamic_concept_witness_selects_one_atomic_fallback() {
    let LoweringOutcome::Unsupported(report) = lower_run(
        r"module dynamic_missing_witness

dyn concept Truth { method truth(self) Bool }

fn missing() dyn Truth { missing() }

pub fn main() Unit {
    discard missing().truth()
    Unit
}
",
    ) else {
        panic!("a missing dynamic witness must not acquire a guessed representation")
    };
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::DynamicWitnessSet),
        "{report:?}"
    );
}

#[test]
fn generic_conditional_dynamic_witness_set_selects_one_atomic_fallback() {
    let LoweringOutcome::Unsupported(report) = lower_run(
        r"module dynamic_conditional_witness

dyn concept Truth { method truth(self) Bool }
record Atom { value Bool }
record Wrapped[T] { value T }

impl Truth for Atom {
    method truth(self) Bool { self.value }
}

impl[T: Truth] Truth for Wrapped[T] {
    method truth(self) Bool { self.value.truth() }
}

fn erase(value Wrapped[Atom]) dyn Truth { value }

pub fn main() Unit {
    discard erase(Wrapped { value = Atom { value = true } }).truth()
    Unit
}
",
    ) else {
        panic!("a generic prerequisite-dependent dynamic catalog must fail closed")
    };
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::DynamicWitnessSet),
        "{report:?}"
    );
}

#[test]
fn test_roots_share_one_reachable_generic_instance() {
    let mir = compile(
        r"module generic_tests

fn identity[T](value T) T { value }

test fn first() Unit {
    discard identity(1)
    Unit
}

test fn second() Unit {
    discard identity(2)
    Unit
}
",
    );
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower generic test artifact");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("generic test roots should lower completely")
    };
    assert_eq!(artifact.test_roots().expect("test roots").len(), 2);
    assert_eq!(
        artifact
            .program()
            .as_program()
            .instances()
            .entries()
            .iter()
            .filter(|instance| instance.key().type_arguments() == [loom_mir::Type::Int])
            .count(),
        1
    );
}

#[test]
fn unreachable_generic_definitions_do_not_change_the_complete_route() {
    let dump = complete_dump(
        r"module unreachable_generic

fn spiral[T](value T) Unit {
    spiral((value, value))
}

pub fn main() Unit { Unit }
",
    );
    assert_eq!(dump.matches("instance ").count(), 1, "{dump}");
    assert!(!dump.contains("source=f0"), "{dump}");
}

#[test]
fn unreachable_task_policy_items_create_no_executor_or_artifact_identity_edge() {
    let any_source = r"module dead_task_policy

pub fn main() {}

async fn child() Int { 1 }

async fn deadPolicy() {
    discard Task.any(child(), child()).await
}
";
    let race_source = r"module dead_task_policy

pub fn main() {}

async fn child() Int { 1 }

async fn deadPolicy() {
    discard Task.race(child(), child()).await
}
";
    let sync_source = r"module dead_task_policy

pub fn main() {}

async fn child() Int { 1 }

fn deadFactory() Task[Int] {
    child()
}
";
    let LoweringOutcome::Complete(any) = lower_run(any_source) else {
        panic!("an unreachable Task.any policy must not select fallback")
    };
    let LoweringOutcome::Complete(race) = lower_run(race_source) else {
        panic!("an unreachable Task.race policy must not select fallback")
    };
    let LoweringOutcome::Complete(sync) = lower_run(sync_source) else {
        panic!("an unreachable synchronous Task helper must not select fallback")
    };
    assert_eq!(dump_program(any.program()), dump_program(race.program()));
    assert_eq!(dump_program(any.program()), dump_program(sync.program()));
    assert_eq!(artifact_identity(&any), artifact_identity(&race));
    assert_eq!(artifact_identity(&any), artifact_identity(&sync));
    for artifact in [&any, &race, &sync] {
        assert_eq!(artifact.functions().len(), 1);
        assert!(
            artifact
                .functions()
                .iter()
                .all(|function| !function.effects().contains(Effects::NEEDS_EXECUTOR))
        );
    }
    let dump = dump_program(any.program());
    assert!(!dump.contains("deadPolicy"), "{dump}");
    assert!(!dump.contains("deadFactory"), "{dump}");
    assert!(!dump.contains("await_tasks"), "{dump}");
}

#[test]
fn user_methods_on_a_value_named_task_remain_plain_reachable_calls() {
    let outcome = lower_run(
        r"module user_task_methods

record Scheduler {}

impl Scheduler {
    method any(self, value Int) Int { value }
    method race(self, value Int) Int { value }
}

pub fn main() Unit {
    let Task = Scheduler {}
    discard Task.any(1)
    discard Task.race(2)
}
",
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("ordinary user methods named like Task policies must lower completely")
    };
    let dump = dump_program(artifact.program());
    assert!(!dump.contains("await_tasks"), "{dump}");
    assert!(!dump.contains("task.join"), "{dump}");
    assert!(
        artifact
            .functions()
            .iter()
            .all(|function| !function.effects().contains(Effects::NEEDS_EXECUTOR))
    );
    assert_eq!(artifact.functions().len(), 3, "{dump}");
}

#[test]
fn nonregular_generic_recursion_selects_atomic_unsupported() {
    let outcome = lower_run(
        r"module nonregular_generic

fn spiral[T](value T) Unit {
    spiral((value, value))
}

pub fn main() Unit {
    spiral(1)
}
",
    );
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("nonregular generic recursion must select whole-artifact fallback")
    };
    assert_eq!(report.len(), 1, "{report:?}");
    assert_eq!(
        report.items()[0].feature(),
        UnsupportedFeature::NonRegularGenericRecursion
    );
}

#[test]
fn oversized_generic_call_key_is_rejected_before_lcir_allocation() {
    let values = std::iter::repeat_n("value", 256)
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "module generic_budget\n\nfn expand[T](value T) Unit {{\n    expand(({values}))\n}}\n\npub fn main() Unit {{\n    expand(1)\n}}\n"
    );
    let outcome = lower_run(&source);
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("oversized concrete instance key must select atomic fallback")
    };
    assert_eq!(report.len(), 1, "{report:?}");
    assert_eq!(
        report.items()[0].feature(),
        UnsupportedFeature::GenericInstanceBudget
    );
}

#[test]
fn sema_valid_result_test_root_is_supported_with_an_explicit_outcome() {
    let mir = compile(
        r"module fallible_tests

enum Problem { Failed }

test fn fallible() Result[Unit, Problem] { Ok(Unit) }
",
    );
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect("a sema-valid test signature must reach coverage classification");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("closed Result-returning test should use typed LCIR")
    };
    assert_eq!(
        artifact.test_outcomes(),
        Some(
            [loom_codegen_ir::TestOutcomePlan::Result {
                success_variant: 0,
                failure_variant: 1,
            }]
            .as_slice()
        )
    );
}

#[test]
fn sema_invalid_test_return_is_an_invalid_root_not_fallback() {
    use loom_mir::{
        Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, Program, Type,
    };

    let span = loom_core::Span::default();
    let mut invalid_test = Function {
        id: FunctionId(0),
        name: "manual.invalid_test".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Bool,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Bool(true)),
                Type::Bool,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    invalid_test
        .renumber_expr_ids()
        .expect("number invalid test root");
    let mir = Program {
        functions: vec![invalid_test],
        tests: vec![FunctionId(0)],
        ..Program::default()
    }
    .into_checked()
    .expect("checked MIR permits the command boundary to validate test returns");

    let error = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect_err("a Bool-returning test cannot select fallback");
    assert_eq!(
        error.code(),
        LoweringErrorCode::InvalidRoot(InvalidRootCode::RootSignature)
    );
}

#[test]
fn hidden_run_root_inputs_are_invalid_not_unsupported() {
    use loom_mir::{
        Block, CallPlan, ConceptDef, ConceptId, Constant, Expr, ExprKind, Function, FunctionId,
        LocalDecl, LocalId, Program, Receiver, Type, WitnessParam,
    };

    let span = loom_core::Span::default();
    let unit_root = || Function {
        id: FunctionId(0),
        name: "manual.main".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    let assert_invalid = |mut function: Function, concepts: Vec<ConceptDef>| {
        function
            .renumber_expr_ids()
            .expect("number invalid run root");
        let mir = Program {
            functions: vec![function],
            concepts,
            exports: std::collections::BTreeMap::from([("main".into(), FunctionId(0))]),
            ..Program::default()
        }
        .into_checked()
        .expect("hidden run inputs are valid inside checked MIR");
        let error = lower_typed_artifact(
            &mir,
            &SourceArtifactRequest::Run {
                entry: "main".into(),
            },
            TargetLayout::new(64).expect("test target"),
        )
        .expect_err("a run harness cannot supply hidden inputs");
        assert_eq!(
            error.code(),
            LoweringErrorCode::InvalidRoot(InvalidRootCode::RootSignature)
        );
    };

    let mut generic = unit_root();
    generic.type_parameters = 1;
    assert_invalid(generic, Vec::new());

    let mut witnessed = unit_root();
    witnessed.witness_params.push(WitnessParam {
        target: Type::Int,
        concept: ConceptId(0),
        bindings: std::collections::BTreeMap::new(),
        span,
    });
    assert_invalid(
        witnessed,
        vec![ConceptDef {
            id: ConceptId(0),
            module: "test".into(),
            name: "Marker".into(),
            span,
            identity: None,
            dynamic: true,
            associated_types: Vec::new(),
            requirements: Vec::new(),
        }],
    );

    let mut inherent = unit_root();
    inherent.params.push(LocalDecl {
        id: LocalId(0),
        name: "self".into(),
        ty: Type::Int,
        mutable: false,
        span,
    });
    inherent.receiver = Some(Receiver::Readonly);
    assert_invalid(inherent, Vec::new());
}

#[test]
fn invalid_run_name_is_an_error_not_unsupported() {
    let mir = compile("module roots\n\npub fn main() Unit { Unit }\n");
    let error = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "missing".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect_err("unknown entry must fail");
    assert_eq!(
        error.code(),
        LoweringErrorCode::InvalidRoot(InvalidRootCode::UnknownEntry)
    );
}

#[test]
fn unreachable_concat_does_not_select_managed_text_or_fallback() {
    let dump = complete_dump(
        r#"module unreachable

fn deadText() Text { "left".concat("right") }

pub fn main() Unit { Unit }
"#,
    );
    assert!(dump.contains("fn i0 mir=f1 \"unreachable.main\""), "{dump}");
    assert!(!dump.contains("deadText"), "{dump}");
}

#[test]
fn unreachable_code_inside_a_reachable_function_is_ignored_exactly() {
    let outcome = lower_run(
        r#"module dead_control

fn helper() Unit { Unit }

pub fn main() Unit {
    return Unit
    let legacy = "legacy"
    discard legacy
    helper()
    Unit
}
"#,
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("dead unsupported values and calls must not select fallback")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.contains("dead_control.main"), "{dump}");
    assert!(!dump.contains("dead_control.helper"), "{dump}");
    assert!(!dump.contains("text"), "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn diverging_prefixes_do_not_require_unmaterialized_unsupported_heads() {
    use loom_mir::{
        Block, CallArgument, CallPlan, CallTarget, Constant, Expr, ExprKind, Function, FunctionId,
        LocalDecl, LocalId, Program, Statement, StatementKind, Type,
    };

    let span = loom_core::Span::default();
    let never_return = || {
        Expr::new(
            ExprKind::Block(Block {
                statements: vec![Statement {
                    kind: StatementKind::Return(Some(Expr::new(
                        ExprKind::Constant(Constant::Unit),
                        Type::Unit,
                        span,
                    ))),
                    span,
                }],
                tail: None,
                span,
            }),
            Type::Never,
            span,
        )
    };
    let root = |id: u32, name: &str, statement: Statement| {
        let mut function = Function {
            id: FunctionId(id),
            name: name.into(),
            span,
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: Vec::new(),
            return_ty: Type::Unit,
            receiver: None,
            body: Block {
                statements: vec![statement],
                tail: Some(Box::new(Expr::new(
                    ExprKind::Constant(Constant::Unit),
                    Type::Unit,
                    span,
                ))),
                span,
            },
            call_plan: CallPlan::default(),
        };
        function.renumber_expr_ids().expect("number dead head");
        function
    };

    let call = root(
        0,
        "manual.dead_call_head",
        Statement {
            kind: StatementKind::Evaluate(Expr::new(
                ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(5)),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(never_return())],
                    witnesses: Vec::new(),
                },
                Type::Text,
                span,
            )),
            span,
        },
    );
    let tuple = root(
        1,
        "manual.dead_tuple_head",
        Statement {
            kind: StatementKind::Evaluate(Expr::new(
                ExprKind::Tuple(vec![
                    never_return(),
                    Expr::new(
                        ExprKind::Constant(Constant::Text("dead".into())),
                        Type::Text,
                        span,
                    ),
                ]),
                Type::Tuple(vec![Type::Never, Type::Text]),
                span,
            )),
            span,
        },
    );
    let list = root(
        2,
        "manual.dead_list_head",
        Statement {
            kind: StatementKind::Evaluate(Expr::new(
                ExprKind::List(vec![
                    never_return(),
                    Expr::new(
                        ExprKind::Constant(Constant::Text("dead".into())),
                        Type::Text,
                        span,
                    ),
                ]),
                Type::List(Box::new(Type::Text)),
                span,
            )),
            span,
        },
    );
    let returning_branch = || Block {
        statements: vec![Statement {
            kind: StatementKind::Return(Some(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        }],
        tail: None,
        span,
    };
    let conditional = root(
        3,
        "manual.dead_if_result",
        Statement {
            kind: StatementKind::Evaluate(Expr::new(
                ExprKind::If {
                    condition: Box::new(Expr::new(
                        ExprKind::Constant(Constant::Bool(true)),
                        Type::Bool,
                        span,
                    )),
                    then_branch: returning_branch(),
                    else_branch: returning_branch(),
                },
                Type::Text,
                span,
            )),
            span,
        },
    );
    let assertion = root(
        4,
        "manual.dead_assert_head",
        Statement {
            kind: StatementKind::Assert {
                condition: never_return(),
            },
            span,
        },
    );
    let mut dead_target = Function {
        id: FunctionId(5),
        name: "manual.dead_text_target".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![LocalDecl {
            id: LocalId(0),
            name: "value".into(),
            ty: Type::Unit,
            mutable: false,
            span,
        }],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Text,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Text("dead".into())),
                Type::Text,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    dead_target.renumber_expr_ids().expect("number dead target");
    let mir = Program {
        functions: vec![call, tuple, list, conditional, assertion, dead_target],
        tests: (0..5).map(FunctionId).collect(),
        ..Program::default()
    }
    .into_checked()
    .expect("checked dead-head MIR");

    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("target"),
    )
    .expect("lower dead heads") else {
        panic!("unmaterialized unsupported heads must not select fallback")
    };
    let dump = dump_program(artifact.program());
    assert_eq!(artifact.functions().len(), 5, "{dump}");
    assert!(!dump.contains("dead_text_target"), "{dump}");
    assert!(!dump.contains("const text"), "{dump}");
}

#[test]
fn reachable_concat_is_repeatably_supported_as_one_complete_artifact() {
    let mir = compile(
        r#"module coverage

fn textValue() Text { "left".concat("right") }

pub fn main() Unit {
    let value = textValue()
    Unit
}
"#,
    );
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("classification succeeds");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("reachable Text concat must select whole-artifact LCIR")
    };
    let repeated = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("repeat classification");
    let LoweringOutcome::Complete(repeated) = repeated else {
        panic!("repeated allocating Text classification must remain supported")
    };
    assert_eq!(artifact_identity(&artifact), artifact_identity(&repeated));
    assert!(artifact.functions().iter().any(|function| {
        function
            .instructions()
            .iter()
            .any(|instruction| matches!(instruction.kind(), InstructionKind::TextConcat { .. }))
    }));
}

#[test]
fn scalar_constants_locals_blocks_short_circuit_and_returns_dump_as_typed_ssa() {
    let dump = complete_dump(
        r"module scalar

fn choose(flag Bool, integer Int, decimal Float) Int {
    var selected = 0
    if flag && decimal != 0.0 {
        selected = integer
        Unit
    } else {
        selected = 7
        Unit
    }
    if flag {
        return selected
    } else {
        selected + 1
    }
}

pub fn main() Unit {
    let output = choose(true, 41, -1.5)
    discard output == 41
    Unit
}
",
    );
    for expected in [
        "effects=may_fault",
        "const bool true",
        "const int 41",
        "const float 0x3ff8000000000000",
        "float.compare.unordered_not_equal",
        "float.negate",
        "branch",
        "checked_int.add",
        "invoke i0",
        "int.compare.equal",
    ] {
        assert!(dump.contains(expected), "missing `{expected}`:\n{dump}");
    }
}

#[test]
fn implicit_unit_and_all_explicit_return_branches_are_supported() {
    let dump = complete_dump(
        r"module returns

fn implicitUnit() {}

fn selected(flag Bool) Int {
    if flag {
        return 1
    } else {
        return 2
    }
}

pub fn main() Unit {
    implicitUnit()
    let value = selected(false)
    Unit
}
",
    );
    assert!(dump.contains("\"returns.implicitUnit\""), "{dump}");
    assert!(dump.matches("return").count() >= 4, "{dump}");
    assert!(dump.contains("call i0()"), "{dump}");
}

#[test]
fn pure_recursive_cycle_stays_infallible_and_uses_direct_calls() {
    let dump = complete_dump(
        r"module pure_recursion

fn recurse(flag Bool) Unit {
    if flag {
        recurse(flag)
    } else {
        Unit
    }
}

pub fn main() Unit {
    recurse(false)
}
",
    );
    assert_eq!(dump.matches("effects=none").count(), 2, "{dump}");
    assert!(dump.matches("call i0(").count() >= 2, "{dump}");
    assert!(!dump.contains("invoke"), "{dump}");
    assert!(!dump.contains("resume_fault"), "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn arithmetic_after_a_diverging_operand_does_not_seed_fault_effects() {
    use loom_mir::{
        BinaryOp, Block, CallPlan, CallTarget, Constant, Expr, ExprKind, Function, FunctionId,
        Program, Statement, StatementKind, Type,
    };

    let span = loom_core::Span::default();
    let mut stops = Function {
        id: FunctionId(0),
        name: "manual.stops_before_add".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr::new(
                ExprKind::Binary(
                    BinaryOp::Add,
                    Box::new(Expr::new(
                        ExprKind::Constant(Constant::Int(1)),
                        Type::Int,
                        span,
                    )),
                    Box::new(Expr::new(
                        ExprKind::Block(Block {
                            statements: vec![Statement {
                                kind: StatementKind::Return(Some(Expr::new(
                                    ExprKind::Constant(Constant::Int(7)),
                                    Type::Int,
                                    span,
                                ))),
                                span,
                            }],
                            tail: None,
                            span,
                        }),
                        Type::Never,
                        span,
                    )),
                ),
                Type::Int,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    let mut main = Function {
        id: FunctionId(1),
        name: "manual.main".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(Expr::new(
                    ExprKind::Call {
                        target: CallTarget::Direct(FunctionId(0)),
                        type_arguments: Vec::new(),
                        arguments: Vec::new(),
                        witnesses: Vec::new(),
                    },
                    Type::Int,
                    span,
                )),
                span,
            }],
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    stops.renumber_expr_ids().expect("number diverging add");
    main.renumber_expr_ids().expect("number caller");
    let mir = Program {
        exports: BTreeMap::from([("main".into(), FunctionId(1))]),
        functions: vec![stops, main],
        ..Program::default()
    }
    .into_checked()
    .expect("checked diverging arithmetic MIR");
    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("target"),
    )
    .expect("lower diverging arithmetic") else {
        panic!("diverging arithmetic prefix is scalar-complete")
    };
    let dump = dump_program(artifact.program());
    assert_eq!(dump.matches("effects=none").count(), 2, "{dump}");
    assert!(dump.contains("call i0()"), "{dump}");
    assert!(!dump.contains("checked_int.add"), "{dump}");
    assert!(!dump.contains("invoke"), "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn checked_mir_move_reassignment_and_readonly_inherent_scalar_call_are_supported() {
    use loom_mir::{
        BinaryOp, Block, CallArgument, CallPlan, CallTarget, Constant, Expr, ExprKind, Function,
        FunctionId, LocalDecl, LocalId, Place, Program, Receiver, Statement, StatementKind, Type,
    };

    let span = loom_core::Span::default();
    let mut same = Function {
        id: FunctionId(0),
        name: "manual.same".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![
            LocalDecl {
                id: LocalId(0),
                name: "self".into(),
                ty: Type::Bool,
                mutable: false,
                span,
            },
            LocalDecl {
                id: LocalId(1),
                name: "other".into(),
                ty: Type::Bool,
                mutable: false,
                span,
            },
        ],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Bool,
        receiver: Some(Receiver::Readonly),
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr::new(
                ExprKind::Binary(
                    BinaryOp::Equal,
                    Box::new(Expr::new(
                        ExprKind::Copy(Place::local(LocalId(0))),
                        Type::Bool,
                        span,
                    )),
                    Box::new(Expr::new(
                        ExprKind::Copy(Place::local(LocalId(1))),
                        Type::Bool,
                        span,
                    )),
                ),
                Type::Bool,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    let mut main = Function {
        id: FunctionId(1),
        name: "manual.main".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![
            LocalDecl {
                id: LocalId(0),
                name: "moved".into(),
                ty: Type::Int,
                mutable: true,
                span,
            },
            LocalDecl {
                id: LocalId(1),
                name: "saved".into(),
                ty: Type::Int,
                mutable: false,
                span,
            },
        ],
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: Expr::new(ExprKind::Constant(Constant::Int(1)), Type::Int, span),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: Expr::new(ExprKind::Move(Place::local(LocalId(0))), Type::Int, span),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(LocalId(0)),
                        value: Expr::new(ExprKind::Constant(Constant::Int(2)), Type::Int, span),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr::new(
                        ExprKind::Copy(Place::local(LocalId(1))),
                        Type::Int,
                        span,
                    )),
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr::new(
                        ExprKind::Call {
                            target: CallTarget::Inherent(FunctionId(0)),
                            type_arguments: Vec::new(),
                            arguments: vec![
                                CallArgument::Value(Expr::new(
                                    ExprKind::Constant(Constant::Bool(true)),
                                    Type::Bool,
                                    span,
                                )),
                                CallArgument::Value(Expr::new(
                                    ExprKind::Constant(Constant::Bool(false)),
                                    Type::Bool,
                                    span,
                                )),
                            ],
                            witnesses: Vec::new(),
                        },
                        Type::Bool,
                        span,
                    )),
                    span,
                },
            ],
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    same.renumber_expr_ids().expect("number inherent body");
    main.renumber_expr_ids().expect("number root body");
    let mir = Program {
        exports: BTreeMap::from([("main".into(), FunctionId(1))]),
        functions: vec![same, main],
        ..Program::default()
    }
    .into_checked()
    .expect("checked manual scalar MIR");

    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower manual scalar MIR");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("manual scalar MIR should be supported")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.contains("bool.compare.equal"), "{dump}");
    assert!(dump.contains("call i0("), "{dump}");
    assert_eq!(dump.matches("effects=none").count(), 2, "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn conditional_moves_preserve_only_values_available_on_continuing_paths() {
    use loom_mir::{
        Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, LocalDecl, LocalId, Place,
        Program, Statement, StatementKind, Type,
    };

    let span = loom_core::Span::default();
    let copy = |local, ty| Expr::new(ExprKind::Copy(Place::local(local)), ty, span);
    let moved = |local| Expr::new(ExprKind::Move(Place::local(local)), Type::Int, span);
    let unit = || Expr::new(ExprKind::Constant(Constant::Unit), Type::Unit, span);
    let empty_unit_block = || Block {
        statements: Vec::new(),
        tail: Some(Box::new(unit())),
        span,
    };
    let flag = LocalId(0);
    let preserved = LocalId(1);
    let intersected = LocalId(2);

    let move_then_return = Block {
        statements: vec![
            Statement {
                kind: StatementKind::Evaluate(moved(preserved)),
                span,
            },
            Statement {
                kind: StatementKind::Return(Some(unit())),
                span,
            },
        ],
        tail: None,
        span,
    };
    let move_then_continue = Block {
        statements: vec![Statement {
            kind: StatementKind::Evaluate(moved(intersected)),
            span,
        }],
        tail: Some(Box::new(copy(flag, Type::Bool))),
        span,
    };
    let continue_without_move = Block {
        statements: Vec::new(),
        tail: Some(Box::new(copy(flag, Type::Bool))),
        span,
    };
    let mut main = Function {
        id: FunctionId(0),
        name: "manual.main".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![
            LocalDecl {
                id: flag,
                name: "flag".into(),
                ty: Type::Bool,
                mutable: false,
                span,
            },
            LocalDecl {
                id: preserved,
                name: "preserved".into(),
                ty: Type::Int,
                mutable: false,
                span,
            },
            LocalDecl {
                id: intersected,
                name: "intersected".into(),
                ty: Type::Int,
                mutable: false,
                span,
            },
        ],
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: flag,
                        value: Expr::new(
                            ExprKind::Constant(Constant::Bool(true)),
                            Type::Bool,
                            span,
                        ),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Let {
                        local: preserved,
                        value: Expr::new(ExprKind::Constant(Constant::Int(11)), Type::Int, span),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Let {
                        local: intersected,
                        value: Expr::new(ExprKind::Constant(Constant::Int(22)), Type::Int, span),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr::new(
                        ExprKind::If {
                            condition: Box::new(copy(flag, Type::Bool)),
                            then_branch: move_then_return,
                            else_branch: empty_unit_block(),
                        },
                        Type::Unit,
                        span,
                    )),
                    span,
                },
                // The move occurred only on the terminated arm, so the sole
                // continuing environment must still contain this local.
                Statement {
                    kind: StatementKind::Evaluate(copy(preserved, Type::Int)),
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr::new(
                        ExprKind::If {
                            condition: Box::new(copy(flag, Type::Bool)),
                            then_branch: move_then_continue,
                            else_branch: continue_without_move,
                        },
                        Type::Bool,
                        span,
                    )),
                    span,
                },
            ],
            tail: Some(Box::new(unit())),
            span,
        },
        call_plan: CallPlan::default(),
    };
    main.renumber_expr_ids().expect("number conditional moves");
    let mir = Program {
        exports: BTreeMap::from([("main".into(), FunctionId(0))]),
        functions: vec![main],
        ..Program::default()
    }
    .into_checked()
    .expect("conditional moves satisfy checked-MIR continuation rules");

    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower conditional moves") else {
        panic!("conditional scalar moves should be supported")
    };
    let dump = dump_program(artifact.program());
    assert_eq!(dump.matches("branch ").count(), 2, "{dump}");
    let jumps = dump
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("jump "))
        .collect::<Vec<_>>();
    assert_eq!(jumps.len(), 2, "{dump}");
    assert!(jumps.iter().all(|jump| jump.ends_with("()")), "{dump}");
}

#[test]
fn source_nested_blocks_and_if_arms_preserve_function_local_values() {
    let dump = complete_dump(
        r"module local_flow

fn throughBlock() Int {
    var value = 0
    {
        value = 41
        Unit
    }
    value
}

fn throughBranches(flag Bool) Int {
    var value = 0
    if flag {
        value = 1
        Unit
    } else {
        value = 2
        Unit
    }
    value
}

pub fn main() Unit {
    discard throughBlock()
    discard throughBranches(true)
    Unit
}
",
    );
    assert!(dump.contains("local_flow.throughBlock"), "{dump}");
    assert!(dump.contains("local_flow.throughBranches"), "{dump}");
    assert!(dump.contains("branch"), "{dump}");
}

#[test]
fn canonical_joins_omit_single_path_and_identity_parameters() {
    let dump = complete_dump(
        r"module canonical_join

fn onePath(flag Bool) Int {
    if flag {
        return 1
    } else {
        2
    }
}

fn sameValue(flag Bool, value Int) Int {
    if flag { value } else { value }
}

pub fn main() Unit {
    discard onePath(false)
    discard sameValue(true, 7)
    Unit
}
",
    );
    let one_path = dump
        .split("fn i1")
        .next()
        .expect("onePath function section");
    assert!(!one_path.contains("jump"), "{one_path}");

    let same_value = dump
        .split("fn i1")
        .nth(1)
        .and_then(|rest| rest.split("fn i2").next())
        .expect("sameValue function section");
    assert_eq!(same_value.matches("jump b3()").count(), 2, "{same_value}");
    assert!(same_value.contains("\n  b3:\n"), "{same_value}");
}

#[test]
fn short_circuit_skip_edge_reuses_the_lhs_without_a_constant_block() {
    let dump = complete_dump(
        r"module canonical_short_circuit

fn stopOnRhs(flag Bool) Bool {
    flag && { return flag }
}

pub fn main() Unit {
    discard stopOnRhs(false)
    Unit
}
",
    );
    let function = dump
        .split("fn i1")
        .next()
        .expect("short-circuit function section");
    assert!(function.contains("branch %v0, b1(), b2()"), "{function}");
    assert!(!function.contains("const bool"), "{function}");
    assert!(!function.contains("jump"), "{function}");
    assert_eq!(function.matches("\n  b").count(), 3, "{function}");
}

#[test]
fn range_header_carries_only_values_changed_on_continuing_paths() {
    let dump = complete_dump(
        r"module canonical_range

fn accumulate(limit Int, readonly Int) Int {
    var changed = 0
    for index in 0..limit {
        changed = changed + readonly
        Unit
    }
    changed
}

pub fn main() Unit {
    discard accumulate(3, 2)
    Unit
}
",
    );
    let function = dump.split("fn i1").next().expect("range function section");
    let header = function
        .lines()
        .find(|line| line.trim_start().starts_with("b1("))
        .expect("range header");
    assert_eq!(header.matches(": t3").count(), 2, "{function}");
    let jumps = function
        .lines()
        .filter(|line| line.contains("jump b1("))
        .collect::<Vec<_>>();
    assert_eq!(jumps.len(), 2, "{function}");
    for jump in jumps {
        assert_eq!(jump.matches('%').count(), 2, "{function}");
    }
    assert!(function.contains("int.successor_below"), "{function}");
}

#[test]
fn pure_and_nested_ranges_use_proved_successors_without_fault_effects() {
    let dump = complete_dump(
        r"module pure_ranges

fn lastBelow(limit Int) Int {
    var last = 0
    for index in 0..limit {
        last = index
        Unit
    }
    last
}

fn nested(outer Int, inner Int) Int {
    var last = 0
    for first in 0..outer {
        for second in 0..inner {
            last = second
            Unit
        }
        last = first
        Unit
    }
    last
}

pub fn main() Unit {
    discard lastBelow(8)
    discard nested(3, 4)
    Unit
}
",
    );

    assert_eq!(dump.matches("effects=none").count(), 3, "{dump}");
    assert_eq!(dump.matches("int.successor_below").count(), 3, "{dump}");
    assert!(!dump.contains("checked_int.add"), "{dump}");
    assert!(!dump.contains("invoke"), "{dump}");
    assert!(!dump.contains("resume_fault"), "{dump}");
    assert_eq!(dump.matches("call i").count(), 2, "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn checked_mir_locals_initialized_in_a_block_or_both_if_arms_survive() {
    use loom_mir::{
        Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, LocalDecl, LocalId, Place,
        Program, Statement, StatementKind, Type,
    };

    let span = loom_core::Span::default();
    let unit = || Expr::new(ExprKind::Constant(Constant::Unit), Type::Unit, span);
    let integer = |value| Expr::new(ExprKind::Constant(Constant::Int(value)), Type::Int, span);
    let copy_int = |local| Expr::new(ExprKind::Copy(Place::local(local)), Type::Int, span);
    let let_integer = |local, value| Statement {
        kind: StatementKind::Let {
            local,
            value: integer(value),
        },
        span,
    };

    let block_local = LocalId(0);
    let branch_local = LocalId(1);
    let nested = Expr::new(
        ExprKind::Block(Block {
            statements: vec![let_integer(block_local, 7)],
            tail: Some(Box::new(unit())),
            span,
        }),
        Type::Unit,
        span,
    );
    let branch = Expr::new(
        ExprKind::If {
            condition: Box::new(Expr::new(
                ExprKind::Constant(Constant::Bool(true)),
                Type::Bool,
                span,
            )),
            then_branch: Block {
                statements: vec![let_integer(branch_local, 11)],
                tail: Some(Box::new(unit())),
                span,
            },
            else_branch: Block {
                statements: vec![let_integer(branch_local, 13)],
                tail: Some(Box::new(unit())),
                span,
            },
        },
        Type::Unit,
        span,
    );
    let mut function = Function {
        id: FunctionId(0),
        name: "manual.main".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![
            LocalDecl {
                id: block_local,
                name: "from_block".into(),
                ty: Type::Int,
                mutable: false,
                span,
            },
            LocalDecl {
                id: branch_local,
                name: "from_branches".into(),
                ty: Type::Int,
                mutable: false,
                span,
            },
        ],
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(nested),
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(copy_int(block_local)),
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(branch),
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(copy_int(branch_local)),
                    span,
                },
            ],
            tail: Some(Box::new(unit())),
            span,
        },
        call_plan: CallPlan::default(),
    };
    function.renumber_expr_ids().expect("number local-flow MIR");
    let mir = Program {
        exports: BTreeMap::from([("main".into(), FunctionId(0))]),
        functions: vec![function],
        ..Program::default()
    }
    .into_checked()
    .expect("checked local-flow MIR");

    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower local-flow MIR");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("function-scoped scalar locals should be supported")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.contains("const int 7"), "{dump}");
    assert!(dump.contains("const int 11"), "{dump}");
    assert!(dump.contains("const int 13"), "{dump}");
    assert!(dump.contains("branch"), "{dump}");
}

#[test]
fn structurally_recursive_fibonacci_uses_checked_edges_and_recursive_invokes() {
    let dump = complete_dump(
        r"module recursive_fib

fn fibonacci(value Int) Int {
    if value < 2 {
        value
    } else {
        fibonacci(value - 1) + fibonacci(value - 2)
    }
}

pub fn main() Unit {
    let output = fibonacci(8)
    Unit
}
",
    );
    let fibonacci = dump.split("fn i1").next().expect("first function dump");
    assert!(fibonacci.contains("int.compare.less"), "{dump}");
    assert!(
        fibonacci.matches("checked_int.subtract").count() >= 2,
        "{dump}"
    );
    assert!(fibonacci.matches("invoke i0").count() >= 2, "{dump}");
    assert!(fibonacci.contains("checked_int.add"), "{dump}");
    assert!(dump.contains("resume_fault"), "{dump}");
}

#[test]
fn structurally_iterative_fibonacci_lowers_for_range_and_loop_carried_assignments() {
    let dump = complete_dump(
        r"module iterative_fib

fn fibonacci(limit Int) Int {
    var previous = 0
    var current = 1
    for index in 0..limit {
        let next = previous + current
        previous = current
        current = next
        Unit
    }
    previous
}

pub fn main() Unit {
    let output = fibonacci(8)
    Unit
}
",
    );
    assert!(dump.contains("int.compare.less"), "{dump}");
    assert!(dump.contains("checked_int.add"), "{dump}");
    assert!(dump.contains("int.successor_below"), "{dump}");
    assert!(dump.contains("jump b"), "{dump}");
    assert!(dump.contains("invoke i0"), "{dump}");
    assert!(dump.contains("resume_fault"), "{dump}");
}

#[test]
fn canonical_direct_local_list_loop_carries_a_trusted_unique_certificate() {
    let dump = complete_dump(
        r"module list_unique_loop

pub fn main() Unit {
    var values = List[Int]()
    for index in 0..128 {
        values.add(index)
        Unit
    }
    let count = values.length()
    Unit
}
",
    );
    assert_eq!(dump.matches("list.append.unique").count(), 1, "{dump}");
    assert!(!dump.contains("list.append %"), "{dump}");
}

#[test]
fn assert_and_defer_lower_to_direct_lexical_cleanup_control_flow() {
    let dump = complete_dump(
        r"module cleanup

fn check(condition Bool) Unit {
    defer { Unit }
    assert condition
    Unit
}

pub fn main() Unit {
    check(true)
    Unit
}
",
    );
    assert!(dump.contains("assert "), "{dump}");
    assert!(dump.contains("contract AssertionFault"), "{dump}");
    assert!(dump.contains("resume_fault"), "{dump}");
}

#[test]
fn source_contracts_lower_to_checked_call_boundaries_and_assumed_bodies() {
    let mir = compile(
        r"module contract_fallback

fn positive(value Int) Int
    requires value > 0
    ensures result > 0
{
    value
}

pub fn main() Unit {
    discard positive(1)
    Unit
}
",
    );
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("classify contracts");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("supported source contracts must lower completely")
    };
    let dump = dump_program(artifact.program());
    assert!(
        dump.contains("contract PreconditionFault")
            && dump.contains("contract PostconditionFault")
            && dump.contains("invoke i0"),
        "{dump}"
    );
}

#[test]
fn closed_pod_records_lower_to_products_with_direct_and_fault_writebacks() {
    let dump = complete_dump(
        r"module product_records

record Counter { total Int, calls Int }
record Holder { counter Counter, enabled Bool }

impl Counter {
    method reset(mut self) Unit {
        self.total = 0
        Unit
    }

    method add(mut self, value Int) Unit {
        self.total = self.total + value
        self.calls = self.calls + 1
        Unit
    }
}

impl Holder {
    method setTotal(mut self, value Int) Unit {
        self.counter.total = value
        Unit
    }
}

fn make() Holder {
    var holder = Holder {
        counter = Counter { total = 1, calls = 2 },
        enabled = true,
    }
    holder.setTotal(3)
    holder
}

pub fn main() Unit {
    var counter = Counter { total = 0, calls = 0 }
    counter.reset()
    counter.add(4)
    discard counter.total
    let holder = make()
    discard holder.counter.total
    Unit
}
",
    );

    assert!(dump.contains("product p0(t3, t3)"), "{dump}");
    assert!(dump.contains("product p1(t5, t2)"), "{dump}");
    assert!(dump.contains("registration k5 = Nominal#"), "{dump}");
    assert!(dump.contains("=> t5"), "{dump}");
    assert!(dump.contains("registration k6 = Nominal#"), "{dump}");
    assert!(dump.contains("=> t6"), "{dump}");
    assert!(dump.contains("product.construct"), "{dump}");
    assert!(dump.contains("product.extract"), "{dump}");
    assert!(dump.contains("product.insert"), "{dump}");
    assert!(dump.contains("inout=[0]"), "{dump}");
    assert!(dump.contains("writebacks("), "{dump}");
    assert!(dump.contains(" = call "), "{dump}");
    assert!(dump.contains("invoke "), "{dump}");
}

#[test]
fn structural_tuples_and_records_lower_through_one_direct_aggregate_plan() {
    let dump = complete_dump(
        r"module tuple_products

record Packet { pair (Int, Bool) }

fn rearrange(input (Packet, Float)) (Bool, Packet) {
    let packet, ignored = input
    let number, enabled = packet.pair
    discard ignored
    (enabled, Packet { pair = (number + 1, enabled) })
}

pub fn main() Unit {
    let enabled, packet = rearrange((Packet { pair = (40, true) }, 1.5))
    discard enabled
    let number, copied = packet.pair
    discard number
    discard copied
    Unit
}
",
    );

    assert!(dump.contains("Tuple[Int,Bool]"), "{dump}");
    assert!(dump.contains("Tuple[Nominal#"), "{dump}");
    assert!(dump.contains("Tuple[Bool,Nominal#"), "{dump}");
    assert!(dump.matches("product.construct").count() >= 4, "{dump}");
    assert!(dump.matches("product.extract").count() >= 6, "{dump}");
    assert!(dump.contains("checked_int.add"), "{dump}");
}

#[test]
fn structural_equality_lowers_products_refinements_and_active_sum_payloads() {
    let dump = complete_dump(
        r"module structural_equality

record Pair {
    number Int
    enabled Bool
}

record Boxed[T] {
    value T
}

type PositivePair = Pair where self.number >= 0

enum Choice {
    Empty
    PairValue(Pair)
    BoxValue(Boxed[Int])
}

fn preserve_pair(value Pair, expected Pair) Pair
    requires value == expected
    ensures result == value
{
    expected
}

fn preserve_choice(value Choice, expected Choice) Choice
    requires value == expected
    ensures result == value
{
    expected
}

fn preserve_pairs(value List[Pair], expected List[Pair]) List[Pair]
    requires value == expected
    ensures result == value
{
    expected
}

pub fn main() Unit {
    let pair = Pair { number = 7, enabled = true }
    let same_pair = Pair { number = 7, enabled = true }
    let other_pair = Pair { number = 8, enabled = true }
    let boxed = Boxed { value = 7 }
    let same_boxed = Boxed { value = 7 }
    let positive = PositivePair(pair)
    let same_positive = PositivePair(same_pair)
    let tuple = (pair, boxed)
    let same_tuple = (same_pair, Boxed { value = 7 })
    let choice = Choice.PairValue(pair)
    let same_choice = Choice.PairValue(same_pair)
    let other_choice = Choice.BoxValue(boxed)
    let checked_pair = preserve_pair(pair, same_pair)
    let checked_choice = preserve_choice(choice, same_choice)
    let pairs = [pair, other_pair]
    let same_pairs = [same_pair, Pair { number = 8, enabled = true }]
    let other_pairs = [same_pair]
    let checked_pairs = preserve_pairs(pairs, same_pairs)
    let nested = [[1, 2], [3]]
    let same_nested = [[1, 2], [3]]
    if pair == same_pair && pair != other_pair && boxed == same_boxed && positive == same_positive && tuple == same_tuple && choice == same_choice && choice != other_choice && checked_pair == pair && checked_choice == choice && pairs == same_pairs && pairs != other_pairs && checked_pairs == pairs && nested == same_nested {
        Unit
    } else {
        discard 1 / 0
        Unit
    }
}
",
    );

    assert!(dump.matches("product.extract").count() >= 12, "{dump}");
    assert!(dump.matches("sum.switch").count() >= 4, "{dump}");
    assert!(dump.contains("unrefine"), "{dump}");
    assert!(dump.contains("int.compare.equal"), "{dump}");
    assert!(dump.contains("bool.compare.equal"), "{dump}");
    assert!(dump.contains("bool.not"), "{dump}");
    assert!(dump.matches("list.length").count() >= 6, "{dump}");
    assert!(dump.matches("list.get").count() >= 6, "{dump}");
    assert!(dump.contains("int.successor_below"), "{dump}");
}

#[test]
fn text_map_contains_remove_and_nested_equality_lower_to_exact_typed_operations() {
    let dump = complete_dump(
        r#"module typed_text_map_operations

record Pair { label Text, count Int }

enum Choice {
    Number(Int)
    Pairing(Pair)
}

pub fn main() Unit {
    let pair = Pair { label = "pair", count = 7 }
    let left = TextMap[Choice]().insert("z", Choice.Number(9)).insert("a", Choice.Pairing(pair))
    let right = TextMap[Choice]().insert("a", Choice.Pairing(pair)).insert("z", Choice.Number(9))
    let missing = left.remove("missing")
    let removed = left.remove("z")
    let nestedLeft = TextMap[TextMap[Choice]]().insert("inner", left)
    let nestedRight = TextMap[TextMap[Choice]]().insert("inner", right)
    let listLeft = TextMap[List[Text]]().insert("items", ["one", "two"])
    let listRight = TextMap[List[Text]]().insert("items", ["one", "two"])
    discard left.contains("a")
    discard !left.contains("missing")
    discard missing == left
    discard removed != left
    discard left == right
    discard nestedLeft == nestedRight
    discard listLeft == listRight
    Unit
}
"#,
    );

    assert!(dump.contains("text_map.contains"), "{dump}");
    assert_eq!(dump.matches("text_map.remove").count(), 2, "{dump}");
    assert!(dump.matches("text_map.entry_get").count() >= 2, "{dump}");
    assert!(dump.matches("text_map.length").count() >= 2, "{dump}");
    assert!(dump.contains("list.get"), "{dump}");
    assert!(dump.contains("text.compare.equal"), "{dump}");
    assert!(!dump.contains("dynamic"), "{dump}");
}

#[test]
fn recursive_list_backed_structural_equality_remains_one_atomic_fallback() {
    let outcome = lower_run(
        r"module recursive_equality

record Node {
    children List[Node]
}

pub fn main() Unit {
    let left = Node { children = [] }
    let right = Node { children = [] }
    discard left == right
    Unit
}
",
    );
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("recursive structural equality must not clone an unbounded LCIR CFG")
    };
    assert_eq!(report.items().len(), 1, "{report:?}");
    assert_eq!(
        report.items()[0].feature(),
        UnsupportedFeature::NominalValue
    );
}

#[test]
fn recursive_text_map_backed_structural_equality_remains_one_atomic_fallback() {
    let outcome = lower_run(
        r"module recursive_text_map_equality

record Node {
    children TextMap[Node]
}

pub fn main() Unit {
    let left = Node { children = TextMap[Node]() }
    let right = Node { children = TextMap[Node]() }
    discard left == right
    Unit
}
",
    );
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("recursive TextMap structural equality must not clone an unbounded LCIR CFG")
    };
    assert_eq!(report.items().len(), 1, "{report:?}");
    assert_eq!(
        report.items()[0].feature(),
        UnsupportedFeature::NominalValue
    );
}

#[test]
fn recursive_json_sum_registers_through_list_and_text_map_cycle_breakers() {
    let dump = complete_dump(include_str!("../../../fixtures/lcir-typed-json/main.loom"));
    for required in [
        "managed_text_map",
        "sum.construct variant 0",
        "sum.construct variant 1",
        "sum.construct variant 2",
        "sum.construct variant 3",
        "sum.construct variant 4",
        "sum.construct variant 5",
        "list.construct",
        "list.get",
        "text_map.construct",
        "text_map.insert",
        "text_map.get",
        "sum.switch",
    ] {
        assert!(dump.contains(required), "missing `{required}`:\n{dump}");
    }
    assert!(
        dump.lines().count() < 10_000,
        "recursive Json lowering must remain finite"
    );
}

#[test]
fn closed_sums_and_ordered_nested_matches_lower_to_exhaustive_sum_cfg() {
    let dump = complete_dump(
        r"module closed_sums

enum Inner {
    Off
    On(Int)
}

enum Choice {
    Empty
    Value(Int)
    Nested(Inner)
    Pair(Int, Bool)
}

fn choose(input Choice) Int {
    match input {
        Value(0) => 10
        Value(value) => value
        Nested(On(value)) => value + 1
        Nested(Off) => 20
        Pair(value, true) => value
        Pair(value, false) => 30
        Empty => 40
    }
}

pub fn main() Unit {
    discard choose(Choice.Value(0))
    discard choose(Choice.Nested(Inner.On(4)))
    discard choose(Choice.Pair(5, true))
    discard choose(Choice.Empty)
    Unit
}
",
    );

    assert!(dump.contains("sum s0 tag=i8"), "{dump}");
    assert!(dump.contains("sum s1 tag=i8"), "{dump}");
    assert!(dump.matches("sum.construct").count() >= 5, "{dump}");
    assert!(dump.matches("sum.switch").count() >= 2, "{dump}");
    assert!(dump.contains("payload0"), "{dump}");
    assert!(dump.contains("int.compare.equal"), "{dump}");
    assert!(dump.contains("bool.compare.equal"), "{dump}");
}

#[test]
fn refined_and_invariant_values_are_direct_sum_payloads() {
    let dump = complete_dump(
        r"module mixed_proven_sums

type Money = Float where self >= 0.0

record Range {
    low Money
    high Money
    invariant self.low <= self.high
}

enum Holding {
    Empty
    Cash(Money)
    Window(Range)
}

fn value(input Holding) Float {
    match input {
        Empty => 0.0
        Cash(money) => money
        Window(range) => {
            discard range
            2.0
        }
    }
}

pub fn main() Unit {
    discard value(Holding.Cash(Money(10.0)))
    discard value(Holding.Window(Range { low = Money(1.0), high = Money(2.0) }))
    Unit
}
",
    );

    assert!(dump.contains("transparent(t4)"), "{dump}");
    assert!(dump.contains("invariant_product"), "{dump}");
    assert!(dump.contains("sum s0"), "{dump}");
    assert!(dump.contains("refine.proven"), "{dump}");
    assert!(dump.contains("invariant_record.proven"), "{dump}");
    assert!(dump.contains("sum.construct"), "{dump}");
    assert!(dump.contains("sum.switch"), "{dump}");
}

#[test]
fn wide_sum_match_shares_one_typed_capturing_arm_block() {
    const VARIANTS: usize = 128;
    let mut variants = String::new();
    for index in 0..VARIANTS {
        writeln!(variants, "    V{index}").expect("variant declaration");
    }
    let source = format!(
        "module wide_sum_dag\n\nenum Wide {{\n{variants}}}\n\nfn classify(input Wide) Int {{\n    match input {{\n        V0 => 0\n        other => 40 + 2\n    }}\n}}\n\npub fn main() Unit {{\n    discard classify(Wide.V127)\n    Unit\n}}\n"
    );
    let mir = compile(&source);
    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower wide sum DAG") else {
        panic!("a bounded wide sum match must remain on typed LCIR")
    };
    let classify = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with(".classify"))
        .expect("lowered classify function");
    assert_eq!(
        classify
            .blocks()
            .iter()
            .filter(|block| matches!(
                block.terminator().map(loom_codegen_ir::Terminator::kind),
                Some(TerminatorKind::CheckedIntBinary {
                    op: CheckedIntBinaryOp::Add,
                    ..
                })
            ))
            .count(),
        1,
        "the shared source arm must be lowered once, not once per enum case"
    );
    let mut incoming_jumps = BTreeMap::new();
    for block in classify.blocks() {
        if let Some(TerminatorKind::Jump(target)) =
            block.terminator().map(loom_codegen_ir::Terminator::kind)
        {
            *incoming_jumps.entry(target.block).or_insert(0_usize) += 1;
        }
    }
    let (shared_arm, incoming) = incoming_jumps
        .into_iter()
        .max_by_key(|(_, incoming)| *incoming)
        .expect("shared arm jump target");
    assert_eq!(incoming, VARIANTS - 1);
    assert_eq!(
        classify
            .block(shared_arm)
            .expect("shared capturing arm")
            .params()
            .len(),
        1,
        "the binding must cross the shared arm edge as one typed SSA parameter"
    );
}

#[test]
fn result_unit_tests_carry_explicit_outcome_plans_through_lowering() {
    let mir = compile(
        r"module result_tests

enum Problem { Failed }

test fn passes() Result[Unit, Problem] { Ok(Unit) }

test fn fails() Result[Unit, Problem] { Err(Problem.Failed) }
",
    );
    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower Result tests") else {
        panic!("closed Result tests should use one LCIR artifact")
    };
    assert_eq!(artifact.test_roots().expect("roots").len(), 2);
    assert_eq!(
        artifact.test_outcomes(),
        Some(
            [
                loom_codegen_ir::TestOutcomePlan::Result {
                    success_variant: 0,
                    failure_variant: 1,
                },
                loom_codegen_ir::TestOutcomePlan::Result {
                    success_variant: 0,
                    failure_variant: 1,
                },
            ]
            .as_slice()
        )
    );
}

#[test]
fn managed_sums_lower_directly_while_unsupported_sum_graphs_fall_back_atomically() {
    let managed = r#"module managed_sum

record Label { value Text }

enum Message { Textual(Label) }

pub fn main() Unit {
    discard Message.Textual(Label { value = "managed" })
    Unit
}
"#;
    assert!(
        matches!(lower_run(managed), LoweringOutcome::Complete(_)),
        "a closed sum with exact managed leaves must lower as one LCIR artifact"
    );
    let list_sum = r"module list_sum

enum Values { Items(List[Int]) }

pub fn main() Unit {
    discard Values.Items(List[Int]())
    Unit
}
";
    assert!(
        matches!(lower_run(list_sum), LoweringOutcome::Complete(_)),
        "a closed sum containing a concrete List must lower as one LCIR artifact"
    );
    let dynamic_sum = r"module dynamic_sum

dyn concept Numbered {
    method number(self) Int
}

record Number { value Int }

impl Numbered for Number {
    method number(self) Int { self.value }
}

enum Packet { Item(dyn Numbered) }

fn erase(value Number) dyn Numbered { value }

pub fn main() Unit {
    discard Packet.Item(erase(Number { value = 1 }))
    Unit
}
";
    let LoweringOutcome::Complete(dynamic_sum) = lower_run(dynamic_sum) else {
        panic!("a closed sum with one exact dynamic witness must lower directly")
    };
    assert!(!dump_program(dynamic_sum.program()).contains("View["));

    for source in [
        r"module recursive_sum

enum Chain {
    End
    Next(Chain)
}

pub fn main() Unit {
    discard Chain.End
    Unit
}
",
        r"module task_sum

enum Work { Pending(Task[Int]) }

async fn child() Int { 1 }

pub async fn main() Unit {
    let work = Work.Pending(child())
    match work {
        Pending(task) => { discard task.await }
    }
    Unit
}
",
    ] {
        let LoweringOutcome::Unsupported(report) = lower_run(source) else {
            panic!("unsupported sum graph must select atomic fallback")
        };
        assert!(report.items().iter().any(|item| matches!(
            item.feature(),
            UnsupportedFeature::ExpressionType
                | UnsupportedFeature::NominalValue
                | UnsupportedFeature::TextConstant
                | UnsupportedFeature::ListValue
                | UnsupportedFeature::RefinedValue
                | UnsupportedFeature::View
                | UnsupportedFeature::AsyncFunction
                | UnsupportedFeature::TaskOperation
        )));
    }
}

const NON_REGULAR_SUM_LOWERING_CHILD_ENV: &str = "LOOM_LCIR_NON_REGULAR_SUM_CHILD";

#[test]
fn non_regular_generic_sum_lowering_finishes_within_the_resource_gate() {
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "non_regular_generic_sum_lowering_child",
            "--nocapture",
        ])
        .env(NON_REGULAR_SUM_LOWERING_CHILD_ENV, "1")
        .spawn()
        .expect("spawn non-regular sum lowering child");
    let status = child
        .wait_timeout(Duration::from_secs(15))
        .expect("wait for non-regular sum lowering child");
    let Some(status) = status else {
        child.kill().expect("kill timed-out sum lowering child");
        child.wait().expect("reap timed-out sum lowering child");
        panic!("non-regular generic sum lowering exceeded 15 seconds");
    };
    assert!(status.success(), "non-regular sum lowering child failed");
}

fn checked_non_regular_spiral_fixture() -> loom_mir::CheckedProgram {
    use loom_core::Span;
    use loom_mir::{
        Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, Program, Statement,
        StatementKind, Type, TypeDef, TypeDefKind, TypeId, VariantDef, VariantId,
    };

    let span = Span::default();
    let spiral = TypeId(0);
    let spiral_int = Type::Nominal(spiral, vec![Type::Int]);
    let done = Expr::new(
        ExprKind::Variant {
            ty: spiral,
            type_arguments: vec![Type::Int],
            variant: VariantId(0),
            payload: vec![Expr::new(
                ExprKind::Constant(Constant::Int(0)),
                Type::Int,
                span,
            )],
        },
        spiral_int,
        span,
    );
    let mut main = Function {
        id: FunctionId(0),
        name: "manual.main".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(done),
                span,
            }],
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    main.renumber_expr_ids().expect("number raw MIR fixture");
    Program {
        types: vec![TypeDef {
            id: spiral,
            name: "Spiral".into(),
            span,
            type_parameters: 1,
            kind: TypeDefKind::Enum {
                variants: vec![
                    VariantDef {
                        id: VariantId(0),
                        name: "Done".into(),
                        payload: vec![Type::Parameter(0)],
                        span,
                    },
                    VariantDef {
                        id: VariantId(1),
                        name: "Next".into(),
                        payload: vec![Type::Nominal(
                            spiral,
                            vec![Type::Tuple(vec![Type::Parameter(0), Type::Parameter(0)])],
                        )],
                        span,
                    },
                ],
            },
        }],
        functions: vec![main],
        exports: BTreeMap::from([("main".into(), FunctionId(0))]),
        ..Program::default()
    }
    .into_checked()
    .expect("bounded MIR validation must accept Spiral[Int].Done(0)")
}

fn checked_non_regular_spiral_coroutine_fixture() -> loom_mir::CheckedProgram {
    use loom_core::Span;
    use loom_mir::{
        Block, CallPlan, CallTarget, Expr, ExprKind, Function, FunctionId, Statement,
        StatementKind, SuspensionPoint, Type,
    };

    let span = Span::default();
    let mut program = checked_non_regular_spiral_fixture().into_program();
    let mut child = program.functions.remove(0);
    let StatementKind::Evaluate(done) = child.body.statements.remove(0).kind else {
        panic!("the manual Spiral fixture must construct one value")
    };
    let spiral_int = done.ty.clone();
    child.id = FunctionId(1);
    child.name = "manual.child".into();
    child.is_async = true;
    child.return_ty = spiral_int.clone();
    child.body.statements.clear();
    child.body.tail = Some(Box::new(done));
    child
        .renumber_expr_ids()
        .expect("number raw async child fixture");

    let task = Expr::new(
        ExprKind::Call {
            target: CallTarget::Direct(child.id),
            type_arguments: Vec::new(),
            arguments: Vec::new(),
            witnesses: Vec::new(),
        },
        Type::Task(Box::new(spiral_int.clone())),
        span,
    );
    let awaited = Expr::new(
        ExprKind::Await {
            state: 1,
            task: Box::new(task),
        },
        spiral_int,
        span,
    );
    let mut main = Function {
        id: FunctionId(0),
        name: "manual.main".into(),
        span,
        type_parameters: 0,
        is_async: true,
        suspension_points: vec![SuspensionPoint {
            state: 1,
            span,
            live_locals: Vec::new(),
        }],
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(awaited),
                span,
            }],
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(loom_mir::Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    main.renumber_expr_ids()
        .expect("number raw async root fixture");
    program.functions = vec![main, child];
    program.exports = BTreeMap::from([("main".into(), FunctionId(0))]);
    program
        .into_checked()
        .expect("bounded MIR validation must accept the non-regular async fixture")
}

#[test]
fn non_regular_generic_sum_lowering_child() {
    if std::env::var_os(NON_REGULAR_SUM_LOWERING_CHILD_ENV).is_none() {
        return;
    }

    let outcome = lower_typed_artifact(
        &checked_non_regular_spiral_fixture(),
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("bounded direct aggregate classification");
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("a non-regular by-value sum must select whole-artifact fallback")
    };
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::NominalValue),
        "{report:?}"
    );

    let coroutine = lower_typed_artifact(
        &checked_non_regular_spiral_coroutine_fixture(),
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("bounded coroutine-frame classification");
    let LoweringOutcome::Unsupported(report) = coroutine else {
        panic!("a non-regular coroutine frame must select whole-artifact fallback")
    };
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::SignatureType),
        "{report:?}"
    );
}

#[test]
fn over_budget_match_plans_select_atomic_fallback() {
    for constant_arms in [300_usize, 513] {
        let mut arms = String::new();
        for value in 0..constant_arms {
            writeln!(arms, "        {value} => {value}").expect("write match arm");
        }
        let source = format!(
            "module match_budget_{constant_arms}\n\nfn classify(value Int) Int {{\n    match value {{\n{arms}        _ => 0\n    }}\n}}\n\npub fn main() Unit {{\n    discard classify(42)\n    Unit\n}}\n"
        );
        let LoweringOutcome::Unsupported(report) = lower_run(&source) else {
            panic!("over-budget match must select atomic fallback")
        };
        assert!(report.items().iter().any(|item| {
            item.feature() == UnsupportedFeature::PatternMatch
                && item.path() == "function[0].body.tail"
        }));
    }
}

#[test]
fn managed_tuple_elements_lower_directly_and_over_budget_tuples_fallback() {
    let managed = lower_run(
        r#"module managed_tuple

fn make() (Int, Text) { (1, "legacy") }

pub fn main() Unit {
    let number, label = make()
    discard number
    discard label
    Unit
}
"#,
    );
    let LoweringOutcome::Complete(managed) = managed else {
        panic!("a tuple containing Text must use the direct managed-product route")
    };
    let text = managed
        .representations()
        .type_id(&loom_mir::Type::Text)
        .expect("Text type");
    assert_eq!(
        managed
            .representations()
            .value_type(text)
            .and_then(|ty| managed.representations().repr(ty.repr())),
        Some(&loom_codegen_ir::Repr::ManagedPointer)
    );
    assert!(
        managed
            .functions()
            .iter()
            .flat_map(loom_codegen_ir::Function::instructions)
            .any(|instruction| matches!(
                instruction.kind(),
                InstructionKind::ProductConstruct { .. }
            ))
    );

    let fields = std::iter::repeat_n("Int", 256)
        .collect::<Vec<_>>()
        .join(", ");
    let values = std::iter::repeat_n("0", 256).collect::<Vec<_>>().join(", ");
    let source = format!(
        "module wide_tuple\n\nfn make() ({fields}) {{ ({values}) }}\n\npub fn main() Unit {{\n    discard make()\n    Unit\n}}\n"
    );
    let wide = lower_run(&source);
    let LoweringOutcome::Unsupported(wide) = wide else {
        panic!("an expanded tuple over the direct-product budget must select fallback")
    };
    assert!(wide.items().iter().any(|item| matches!(
        item.feature(),
        UnsupportedFeature::SignatureType | UnsupportedFeature::ExpressionType
    )));
}

#[test]
fn runtime_constraints_build_exact_typed_results_and_structured_errors() {
    let outcome = lower_run(
        r"module invariant_record

record Positive {
    value Int
    invariant self.value + 1 > 0
}

type StrictPositive = Int where self + 1 > 1

fn checked_record(value Int) Result[Positive, ConstraintError] {
    Positive { value = value }
}

fn checked_refined(value Int) Result[StrictPositive, ConstraintError] {
    StrictPositive(value)
}

pub fn main() Unit {
    discard checked_record(1)
    discard checked_refined(1)
    Unit
}
",
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("nongeneric runtime constraints must use typed LCIR: {outcome:?}")
    };
    let instructions = artifact
        .functions()
        .iter()
        .flat_map(loom_codegen_ir::Function::instructions)
        .collect::<Vec<_>>();
    assert!(instructions.iter().any(|instruction| matches!(
        instruction.kind(),
        InstructionKind::InvariantRecordProven { .. }
    )));
    assert!(
        instructions
            .iter()
            .any(|instruction| matches!(instruction.kind(), InstructionKind::RefineProven { .. }))
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(
                instruction.kind(),
                InstructionKind::ListConstruct { elements } if elements.is_empty()
            ))
            .count(),
        2,
        "each rejection path owns an exact empty List[Text] path"
    );
    let text_literals = instructions
        .iter()
        .filter_map(|instruction| match instruction.kind() {
            InstructionKind::TextLiteral { utf8 } => Some(utf8.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for literal in [
        "Positive",
        "StrictPositive",
        "InvariantViolation",
        "ConstraintViolation",
        "Positive.invariant",
        "StrictPositive.constraint",
        "Int",
    ] {
        assert!(
            text_literals.contains(&literal),
            "missing generated ConstraintError literal {literal:?}: {text_literals:?}"
        );
    }
    let dump = dump_program(artifact.program());
    assert!(dump.contains("effects=may_fault"), "{dump}");
    assert!(!dump.contains("loom.Value"), "{dump}");
}

#[test]
fn projection_through_a_protected_product_is_atomic_unsupported() {
    let outcome = lower_run(
        r"module protected_projection

record Positive {
    value Int
    invariant self.value >= 0
}

record Holder { value Positive }

pub fn main() Unit {
    let holder = Holder { value = Positive { value = 7 } }
    discard holder.value.value
    Unit
}
",
    );
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("projection through an invariant product must select whole-artifact fallback")
    };
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::ProjectedPlace),
        "{report:?}"
    );
}

#[test]
fn projected_inout_uses_typed_extraction_and_functional_root_reconstruction() {
    let dump = complete_dump(
        r"module projected_inout

record Counter { value Int }
record Holder { counter Counter }

impl Counter {
    method add(mut self, value Int) Unit {
        self.value = self.value + value
        Unit
    }
}

pub fn main() Unit {
    var holder = Holder { counter = Counter { value = 0 } }
    holder.counter.add(1)
    Unit
}
",
    );
    assert!(dump.contains("product.extract"), "{dump}");
    assert!(dump.contains("product.insert"), "{dump}");
    assert!(dump.contains("inout=[0]"), "{dump}");
    assert!(dump.contains("invoke "), "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn checked_projected_move_extracts_the_leaf_and_consumes_its_root() {
    use loom_core::Span;
    use loom_mir::{
        Block, CallArgument, CallPlan, CallTarget, Constant, ConstructionMode, Expr, ExprKind,
        FieldDef, Function, FunctionId, LocalDecl, LocalId, Place, Program, Statement,
        StatementKind, Type, TypeDef, TypeDefKind, TypeId,
    };

    let span = Span::default();
    let inner = Type::Nominal(TypeId(0), Vec::new());
    let outer = Type::Nominal(TypeId(1), Vec::new());
    let local = |id, ty| LocalDecl {
        id: LocalId(id),
        name: format!("local_{id}"),
        ty,
        mutable: false,
        span,
    };
    let expression = |kind, ty| Expr::new(kind, ty, span);
    let mut take = Function {
        id: FunctionId(0),
        name: "projected_move.take".to_owned(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, outer.clone())],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(expression(
                ExprKind::Move(Place {
                    local: LocalId(0),
                    projection: vec![0, 1],
                }),
                Type::Int,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    take.renumber_expr_ids().expect("take expression ids");
    let inner_value = expression(
        ExprKind::Record {
            ty: TypeId(0),
            type_arguments: Vec::new(),
            fields: vec![
                expression(ExprKind::Constant(Constant::Int(1)), Type::Int),
                expression(ExprKind::Constant(Constant::Int(7)), Type::Int),
            ],
            construction: ConstructionMode::Plain,
        },
        inner.clone(),
    );
    let outer_value = expression(
        ExprKind::Record {
            ty: TypeId(1),
            type_arguments: Vec::new(),
            fields: vec![
                inner_value,
                expression(ExprKind::Constant(Constant::Bool(true)), Type::Bool),
            ],
            construction: ConstructionMode::Plain,
        },
        outer.clone(),
    );
    let mut main = Function {
        id: FunctionId(1),
        name: "projected_move.main".to_owned(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![local(0, outer.clone()), local(1, Type::Int)],
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: outer_value,
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: expression(
                            ExprKind::Call {
                                target: CallTarget::Direct(FunctionId(0)),
                                type_arguments: Vec::new(),
                                arguments: vec![CallArgument::Value(expression(
                                    ExprKind::Move(Place::local(LocalId(0))),
                                    outer.clone(),
                                ))],
                                witnesses: Vec::new(),
                            },
                            Type::Int,
                        ),
                    },
                    span,
                },
            ],
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    main.renumber_expr_ids().expect("main expression ids");
    let checked = Program {
        types: vec![
            TypeDef {
                id: TypeId(0),
                name: "Inner".to_owned(),
                span,
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![
                        FieldDef {
                            name: "left".to_owned(),
                            ty: Type::Int,
                            span,
                        },
                        FieldDef {
                            name: "right".to_owned(),
                            ty: Type::Int,
                            span,
                        },
                    ],
                    invariant: None,
                },
            },
            TypeDef {
                id: TypeId(1),
                name: "Outer".to_owned(),
                span,
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![
                        FieldDef {
                            name: "inner".to_owned(),
                            ty: inner,
                            span,
                        },
                        FieldDef {
                            name: "guard".to_owned(),
                            ty: Type::Bool,
                            span,
                        },
                    ],
                    invariant: None,
                },
            },
        ],
        functions: vec![take, main],
        exports: std::collections::BTreeMap::from([("main".to_owned(), FunctionId(1))]),
        ..Program::default()
    }
    .into_checked()
    .expect("projected move checked MIR");
    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &checked,
        &SourceArtifactRequest::Run {
            entry: "main".to_owned(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower projected move") else {
        panic!("direct projected move must be complete")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.matches("product.extract").count() >= 2, "{dump}");
    assert!(dump.contains(" = call i0"), "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn over_budget_product_depth_and_structure_select_atomic_fallback() {
    use loom_mir::{
        Block, CallArgument, CallPlan, CallTarget, Constant, ConstructionMode, Expr, ExprKind,
        FieldDef, Function, FunctionId, LocalDecl, LocalId, Program, Statement, StatementKind,
        Type, TypeDef, TypeDefKind, TypeId,
    };

    const OVER_BUDGET_RECORDS: usize = 257;
    let span = loom_core::Span::default();
    let nominal = |index: usize| {
        Type::Nominal(
            TypeId(u32::try_from(index).expect("test type identity")),
            Vec::new(),
        )
    };
    let record_type = |index: usize, fields: Vec<FieldDef>| TypeDef {
        id: TypeId(u32::try_from(index).expect("test type identity")),
        name: format!("R{index}"),
        span,
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields,
            invariant: None,
        },
    };
    let function = |id: usize, name: String, result: Type, tail: Expr| {
        let mut function = Function {
            id: FunctionId(u32::try_from(id).expect("test function identity")),
            name,
            span,
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: Vec::new(),
            return_ty: result,
            receiver: None,
            body: Block {
                statements: Vec::new(),
                tail: Some(Box::new(tail)),
                span,
            },
            call_plan: CallPlan::default(),
        };
        function.renumber_expr_ids().expect("number test function");
        function
    };
    let root = |id: usize, record: Type, callee: FunctionId| {
        let call = Expr::new(
            ExprKind::Call {
                target: CallTarget::Direct(callee),
                type_arguments: Vec::new(),
                arguments: Vec::new(),
                witnesses: Vec::new(),
            },
            record.clone(),
            span,
        );
        let mut root = Function {
            id: FunctionId(u32::try_from(id).expect("root identity")),
            name: "manual.main".into(),
            span,
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: vec![LocalDecl {
                id: LocalId(0),
                name: "value".into(),
                ty: record,
                mutable: false,
                span,
            }],
            return_ty: Type::Unit,
            receiver: None,
            body: Block {
                statements: vec![Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: call,
                    },
                    span,
                }],
                tail: Some(Box::new(Expr::new(
                    ExprKind::Constant(Constant::Unit),
                    Type::Unit,
                    span,
                ))),
                span,
            },
            call_plan: CallPlan::default(),
        };
        root.renumber_expr_ids().expect("number root");
        root
    };

    let deep_types = (0..OVER_BUDGET_RECORDS)
        .map(|index| {
            let field = if index + 1 == OVER_BUDGET_RECORDS {
                FieldDef {
                    name: "value".into(),
                    ty: Type::Int,
                    span,
                }
            } else {
                FieldDef {
                    name: "next".into(),
                    ty: nominal(index + 1),
                    span,
                }
            };
            record_type(index, vec![field])
        })
        .collect::<Vec<_>>();
    let mut deep_functions: Vec<Function> = Vec::with_capacity(OVER_BUDGET_RECORDS + 1);
    for index in (0..OVER_BUDGET_RECORDS).rev() {
        let field = if index + 1 == OVER_BUDGET_RECORDS {
            Expr::new(ExprKind::Constant(Constant::Int(0)), Type::Int, span)
        } else {
            let child = deep_functions.last().expect("child factory").id;
            Expr::new(
                ExprKind::Call {
                    target: CallTarget::Direct(child),
                    type_arguments: Vec::new(),
                    arguments: Vec::<CallArgument>::new(),
                    witnesses: Vec::new(),
                },
                nominal(index + 1),
                span,
            )
        };
        let result = nominal(index);
        deep_functions.push(function(
            deep_functions.len(),
            format!("manual.make_r{index}"),
            result.clone(),
            Expr::new(
                ExprKind::Record {
                    ty: TypeId(u32::try_from(index).expect("record identity")),
                    type_arguments: Vec::new(),
                    fields: vec![field],
                    construction: ConstructionMode::Plain,
                },
                result,
                span,
            ),
        ));
    }
    let deep_factory = deep_functions.last().expect("root factory").id;
    deep_functions.push(root(OVER_BUDGET_RECORDS, nominal(0), deep_factory));
    let deep = Program {
        types: deep_types,
        functions: deep_functions,
        exports: BTreeMap::from([(
            "main".into(),
            FunctionId(u32::try_from(OVER_BUDGET_RECORDS).expect("root identity")),
        )]),
        ..Program::default()
    }
    .into_checked()
    .expect("checked deep product graph");

    let wide_id = TypeId(0);
    let wide_types = vec![record_type(
        0,
        (0..OVER_BUDGET_RECORDS)
            .map(|index| FieldDef {
                name: format!("f{index}"),
                ty: Type::Int,
                span,
            })
            .collect(),
    )];
    let wide_type = Type::Nominal(wide_id, Vec::new());
    let wide_fields = (0..OVER_BUDGET_RECORDS)
        .map(|_| Expr::new(ExprKind::Constant(Constant::Int(0)), Type::Int, span))
        .collect();
    let wide_factory = function(
        0,
        "manual.make_wide".into(),
        wide_type.clone(),
        Expr::new(
            ExprKind::Record {
                ty: wide_id,
                type_arguments: Vec::new(),
                fields: wide_fields,
                construction: ConstructionMode::Plain,
            },
            wide_type.clone(),
            span,
        ),
    );
    let wide = Program {
        types: wide_types,
        functions: vec![wide_factory, root(1, wide_type, FunctionId(0))],
        exports: BTreeMap::from([("main".into(), FunctionId(1))]),
        ..Program::default()
    }
    .into_checked()
    .expect("checked wide product graph");

    for mir in [&deep, &wide] {
        let outcome = lower_typed_artifact(
            mir,
            &SourceArtifactRequest::Run {
                entry: "main".into(),
            },
            TargetLayout::new(64).expect("test target"),
        )
        .expect("over-budget product classification");
        let LoweringOutcome::Unsupported(report) = outcome else {
            panic!("an over-budget direct product graph must select atomic fallback")
        };
        assert!(
            report.items().iter().any(|item| matches!(
                item.feature(),
                UnsupportedFeature::SignatureType
                    | UnsupportedFeature::ExpressionType
                    | UnsupportedFeature::NominalValue
            )),
            "{report:?}"
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn over_depth_projected_place_selects_one_atomic_unsupported_outcome() {
    use loom_core::Span;
    use loom_mir::{
        Block, CallArgument, CallPlan, CallTarget, Constant, ConstructionMode, Expr, ExprKind,
        FieldDef, Function, FunctionId, LocalDecl, LocalId, Place, Program, Statement,
        StatementKind, Type, TypeDef, TypeDefKind, TypeId,
    };

    const DEPTH: usize = 65;
    let span = Span::default();
    let nominal = |index: usize| {
        Type::Nominal(
            TypeId(u32::try_from(index).expect("test type identity")),
            Vec::new(),
        )
    };
    let types = (0..DEPTH)
        .map(|index| TypeDef {
            id: TypeId(u32::try_from(index).expect("test type identity")),
            name: format!("Projection{index}"),
            span,
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "field".into(),
                    ty: if index + 1 == DEPTH {
                        Type::Int
                    } else {
                        nominal(index + 1)
                    },
                    span,
                }],
                invariant: None,
            },
        })
        .collect::<Vec<_>>();
    let local = |id, ty| LocalDecl {
        id: LocalId(id),
        name: format!("local_{id}"),
        ty,
        mutable: false,
        span,
    };
    let mut factories: Vec<Function> = Vec::with_capacity(DEPTH);
    for index in (0..DEPTH).rev() {
        let ty = nominal(index);
        let field = if index + 1 == DEPTH {
            Expr::new(ExprKind::Constant(Constant::Int(7)), Type::Int, span)
        } else {
            Expr::new(
                ExprKind::Call {
                    target: CallTarget::Direct(factories.last().expect("child factory").id),
                    type_arguments: Vec::new(),
                    arguments: Vec::new(),
                    witnesses: Vec::new(),
                },
                nominal(index + 1),
                span,
            )
        };
        let mut factory = Function {
            id: FunctionId(u32::try_from(factories.len()).expect("factory identity")),
            name: format!("projected_depth.make_{index}"),
            span,
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: Vec::new(),
            return_ty: ty.clone(),
            receiver: None,
            body: Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr::new(
                    ExprKind::Record {
                        ty: TypeId(u32::try_from(index).expect("record identity")),
                        type_arguments: Vec::new(),
                        fields: vec![field],
                        construction: ConstructionMode::Plain,
                    },
                    ty,
                    span,
                ))),
                span,
            },
            call_plan: CallPlan::default(),
        };
        factory
            .renumber_expr_ids()
            .expect("number aggregate factory");
        factories.push(factory);
    }
    let root_factory = factories.last().expect("root factory").id;
    let mut read = Function {
        id: FunctionId(u32::try_from(DEPTH).expect("read identity")),
        name: "projected_depth.read".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, nominal(0))],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr::new(
                ExprKind::Copy(Place {
                    local: LocalId(0),
                    projection: vec![0; DEPTH],
                }),
                Type::Int,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    read.renumber_expr_ids().expect("number projected read");
    let read_id = read.id;
    let mut main = Function {
        id: FunctionId(u32::try_from(DEPTH + 1).expect("main identity")),
        name: "projected_depth.main".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![local(0, Type::Int)],
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![Statement {
                kind: StatementKind::Let {
                    local: LocalId(0),
                    value: Expr::new(
                        ExprKind::Call {
                            target: CallTarget::Direct(read_id),
                            type_arguments: Vec::new(),
                            arguments: vec![CallArgument::Value(Expr::new(
                                ExprKind::Call {
                                    target: CallTarget::Direct(root_factory),
                                    type_arguments: Vec::new(),
                                    arguments: Vec::new(),
                                    witnesses: Vec::new(),
                                },
                                nominal(0),
                                span,
                            ))],
                            witnesses: Vec::new(),
                        },
                        Type::Int,
                        span,
                    ),
                },
                span,
            }],
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    main.renumber_expr_ids().expect("number root");
    let main_id = main.id;
    factories.push(read);
    factories.push(main);
    let checked = Program {
        types,
        functions: factories,
        exports: BTreeMap::from([("main".into(), main_id)]),
        ..Program::default()
    }
    .into_checked()
    .expect("projection remains within the MIR nesting limit");

    let outcome = lower_typed_artifact(
        &checked,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("bounded projection preflight");
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("an over-depth projected place must select whole-artifact fallback")
    };
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::ProjectedPlace),
        "{report:?}"
    );
}
