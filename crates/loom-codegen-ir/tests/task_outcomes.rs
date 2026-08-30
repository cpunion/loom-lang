use std::collections::BTreeSet;

use loom_codegen_ir::{
    AwaitMode, BlockTarget, CanonicalTypeCatalog, CheckedProgram, CoroutinePlan,
    CoroutineSuspension, Effects, InstanceId, InstructionKind, ManagedSafepoint, Origin, Program,
    ProgramBuilder, ResultTarget, Signature, TargetLayout, Terminator, TerminatorKind,
    UnwindTarget, ValidationCode, ValueDefinition, ValueId, check_program, dump_program,
    plan_managed_roots, validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

const TASK_FAULT_TYPE_ID: TypeId = TypeId(104);
const TASK_OUTCOME_TYPE_ID: TypeId = TypeId(105);

fn task_catalog() -> CanonicalTypeCatalog {
    CanonicalTypeCatalog {
        task_fault: Some(TASK_FAULT_TYPE_ID),
        task_outcome: Some(TASK_OUTCOME_TYPE_ID),
        ..CanonicalTypeCatalog::default()
    }
}

#[derive(Clone, Copy)]
enum FaultShape {
    Canonical,
    WrongMessageType,
}

#[derive(Clone, Copy)]
enum OutcomeShape {
    Canonical,
    WrongNominal,
    WrongCompletedPayload,
}

#[derive(Clone, Copy)]
enum TakeSource {
    Settled,
    Race,
    Created,
    Forwarded,
}

#[derive(Clone, Copy, Debug)]
enum TakePrefix {
    Complete,
    Missing,
    Partial,
    Reversed,
    NonPrefix,
    Duplicate,
}

struct OutcomeProgram {
    program: Program,
    root: InstanceId,
    take_result: ValueId,
    live_text: ValueId,
}

#[expect(
    clippy::too_many_lines,
    reason = "one raw builder keeps the nominal-shape, provenance, effects, and root-plan matrix independently forgeable"
)]
fn outcome_program(
    fault_shape: FaultShape,
    outcome_shape: OutcomeShape,
    source: TakeSource,
    include_collect_effect: bool,
) -> OutcomeProgram {
    let root_origin = Origin::synthetic(FunctionId(0));
    let child_origin = Origin::synthetic(FunctionId(1));
    let mut builder = ProgramBuilder::with_canonical_types(
        TargetLayout::new(64).expect("target"),
        task_catalog(),
    );
    let integer = builder.type_id(&Type::Int).expect("Int");
    let text = builder
        .add_managed_text_type()
        .expect("canonical managed Text");
    let fault_fields = match fault_shape {
        FaultShape::Canonical => vec![Type::Text, Type::Text],
        FaultShape::WrongMessageType => vec![Type::Text, Type::Int],
    };
    let fault_semantic = Type::Nominal(TASK_FAULT_TYPE_ID, Vec::new());
    builder
        .add_pod_record_type(fault_semantic.clone(), &fault_fields)
        .expect("TaskFault product");
    let outcome_id = match outcome_shape {
        OutcomeShape::WrongNominal => TypeId(TASK_OUTCOME_TYPE_ID.0 + 1),
        OutcomeShape::Canonical | OutcomeShape::WrongCompletedPayload => TASK_OUTCOME_TYPE_ID,
    };
    let completed = match outcome_shape {
        OutcomeShape::WrongCompletedPayload => Type::Bool,
        OutcomeShape::Canonical | OutcomeShape::WrongNominal => Type::Int,
    };
    let outcome_semantic = Type::Nominal(outcome_id, vec![Type::Int]);
    let outcome = builder
        .add_sum_type(
            outcome_semantic,
            &[
                Box::from([completed]),
                Box::from([fault_semantic]),
                Box::new([]),
            ],
        )
        .expect("TaskOutcome sum");
    let task_int = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Int)))
        .expect("Task[Int]");

    let child = builder
        .declare_function(
            child_origin,
            "outcome.child",
            Signature::new([], integer),
            Effects::NEEDS_EXECUTOR.with_implications(),
        )
        .expect("child coroutine");
    {
        let mut function = builder.function(child).expect("child builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(integer, []))
            .expect("child plan");
        let entry = function.create_block().expect("child entry");
        function.set_entry(entry).expect("child entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(loom_codegen_ir::Constant::Int(7)),
                &[integer],
                child_origin,
            )
            .expect("Int")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), child_origin),
            )
            .expect("child return");
    }

    let mut effects = Effects::NEEDS_EXECUTOR;
    if include_collect_effect {
        effects = effects.union(Effects::MAY_COLLECT);
    }
    if matches!(source, TakeSource::Settled | TakeSource::Race) {
        effects = effects
            .union(Effects::MAY_FAULT)
            .union(Effects::MAY_SUSPEND);
    }
    let root = builder
        .declare_function(
            root_origin,
            "outcome.root",
            Signature::new([text], outcome),
            effects.with_implications(),
        )
        .expect("root coroutine");
    let (take_result, live_text) = {
        let mut function = builder.function(root).expect("root builder");
        let suspensions = match source {
            TakeSource::Settled => vec![CoroutineSuspension::new(
                1,
                AwaitMode::Settled,
                [integer],
                [text],
            )],
            TakeSource::Race => vec![CoroutineSuspension::new(
                1,
                AwaitMode::Race,
                [integer],
                [text],
            )],
            TakeSource::Created | TakeSource::Forwarded => Vec::new(),
        };
        function
            .set_coroutine_plan(CoroutinePlan::new(outcome, suspensions))
            .expect("root plan");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        let input_text = function
            .append_block_parameter(entry, text)
            .expect("Text parameter");
        let child_task = function
            .append_instruction(
                entry,
                InstructionKind::TaskCreate {
                    coroutine: child,
                    arguments: Box::new([]),
                },
                &[task_int],
                root_origin,
            )
            .expect("Task[Int]")[0];

        let (take_block, terminal_task, live_text) = match source {
            TakeSource::Settled | TakeSource::Race => {
                let normal = function.create_block().expect("normal");
                let fault = function.create_block().expect("fault");
                let cancel = function.create_block().expect("cancel");
                let terminal_task = function
                    .append_block_parameter(normal, task_int)
                    .expect("terminal Task[Int]");
                let live_text = function
                    .append_block_parameter(normal, text)
                    .expect("normal Text");
                function
                    .append_block_parameter(fault, text)
                    .expect("fault Text");
                function
                    .append_block_parameter(cancel, text)
                    .expect("cancel Text");
                function
                    .terminate(
                        entry,
                        Terminator::new(
                            TerminatorKind::AwaitTasks {
                                state: 1,
                                mode: if matches!(source, TakeSource::Settled) {
                                    AwaitMode::Settled
                                } else {
                                    AwaitMode::Race
                                },
                                tasks: Box::from([child_task]),
                                normal: ResultTarget::new(normal, [input_text]),
                                fault: UnwindTarget::new(fault, [input_text]),
                                cancel: BlockTarget::new(cancel, [input_text]),
                            },
                            root_origin,
                        ),
                    )
                    .expect("await terminal child");
                function
                    .terminate(
                        fault,
                        Terminator::new(TerminatorKind::ResumeFault, root_origin),
                    )
                    .expect("resume fault");
                function
                    .terminate(
                        cancel,
                        Terminator::new(TerminatorKind::TaskCancelled, root_origin),
                    )
                    .expect("cancel");
                (normal, terminal_task, live_text)
            }
            TakeSource::Created => (entry, child_task, input_text),
            TakeSource::Forwarded => {
                let forwarded = function.create_block().expect("forwarded");
                let forwarded_task = function
                    .append_block_parameter(forwarded, task_int)
                    .expect("forwarded Task[Int]");
                let forwarded_text = function
                    .append_block_parameter(forwarded, text)
                    .expect("forwarded Text");
                function
                    .terminate(
                        entry,
                        Terminator::new(
                            TerminatorKind::Jump(BlockTarget::new(
                                forwarded,
                                [child_task, input_text],
                            )),
                            root_origin,
                        ),
                    )
                    .expect("forward Task");
                (forwarded, forwarded_task, forwarded_text)
            }
        };
        let take_result = function
            .append_instruction(
                take_block,
                InstructionKind::TaskOutcomeTake {
                    task: terminal_task,
                },
                &[outcome],
                root_origin,
            )
            .expect("TaskOutcome")[0];
        function
            .append_instruction(
                take_block,
                InstructionKind::TextLength { text: live_text },
                &[integer],
                root_origin,
            )
            .expect("post-safepoint Text use");
        function
            .terminate(
                take_block,
                Terminator::new(TerminatorKind::Return(take_result), root_origin),
            )
            .expect("root return");
        (take_result, live_text)
    };

    OutcomeProgram {
        program: builder.finish(),
        root,
        take_result,
        live_text,
    }
}

fn canonical_program(source: TakeSource) -> OutcomeProgram {
    outcome_program(FaultShape::Canonical, OutcomeShape::Canonical, source, true)
}

#[expect(
    clippy::too_many_lines,
    reason = "the hostile raw builder keeps terminal-result width, forwarded live parameters, and instruction order independently forgeable"
)]
fn terminal_prefix_program(mode: AwaitMode, prefix: TakePrefix) -> Program {
    let root_origin = Origin::synthetic(FunctionId(0));
    let child_origin = Origin::synthetic(FunctionId(1));
    let mut builder = ProgramBuilder::with_canonical_types(
        TargetLayout::new(64).expect("target"),
        task_catalog(),
    );
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let text = builder
        .add_managed_text_type()
        .expect("canonical managed Text");
    let fault_semantic = Type::Nominal(TASK_FAULT_TYPE_ID, Vec::new());
    builder
        .add_pod_record_type(fault_semantic.clone(), &[Type::Text, Type::Text])
        .expect("TaskFault product");
    let outcome_semantic = Type::Nominal(TASK_OUTCOME_TYPE_ID, vec![Type::Int]);
    let outcome = builder
        .add_sum_type(
            outcome_semantic,
            &[
                Box::from([Type::Int]),
                Box::from([fault_semantic]),
                Box::new([]),
            ],
        )
        .expect("TaskOutcome sum");
    let task_int = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Int)))
        .expect("Task[Int]");

    let child = builder
        .declare_function(
            child_origin,
            "prefix.child",
            Signature::new([], integer),
            Effects::NEEDS_EXECUTOR.with_implications(),
        )
        .expect("child coroutine");
    {
        let mut function = builder.function(child).expect("child builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(integer, []))
            .expect("child plan");
        let entry = function.create_block().expect("child entry");
        function.set_entry(entry).expect("child entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(loom_codegen_ir::Constant::Int(7)),
                &[integer],
                child_origin,
            )
            .expect("Int")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), child_origin),
            )
            .expect("child return");
    }

    let root = builder
        .declare_function(
            root_origin,
            "prefix.root",
            Signature::new([text], unit),
            Effects::NEEDS_EXECUTOR
                .union(Effects::MAY_COLLECT)
                .union(Effects::MAY_FAULT)
                .union(Effects::MAY_SUSPEND)
                .with_implications(),
        )
        .expect("root coroutine");
    {
        let mut function = builder.function(root).expect("root builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(
                unit,
                [CoroutineSuspension::new(
                    1,
                    mode,
                    [integer, integer],
                    [text],
                )],
            ))
            .expect("root plan");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let fault = function.create_block().expect("fault");
        let cancel = function.create_block().expect("cancel");
        function.set_entry(entry).expect("entry");
        let input_text = function
            .append_block_parameter(entry, text)
            .expect("entry Text");
        let tasks = (0..2)
            .map(|_| {
                function
                    .append_instruction(
                        entry,
                        InstructionKind::TaskCreate {
                            coroutine: child,
                            arguments: Box::new([]),
                        },
                        &[task_int],
                        root_origin,
                    )
                    .expect("Task[Int]")[0]
            })
            .collect::<Vec<_>>();
        let implicit_results = match mode {
            AwaitMode::Settled => 2,
            AwaitMode::Race => 1,
            AwaitMode::All | AwaitMode::Any => panic!("terminal prefix fixture needs settled/race"),
        };
        let terminal_tasks = (0..implicit_results)
            .map(|_| {
                function
                    .append_block_parameter(normal, task_int)
                    .expect("terminal Task[Int]")
            })
            .collect::<Vec<_>>();
        let live_text = function
            .append_block_parameter(normal, text)
            .expect("normal live Text");
        function
            .append_block_parameter(fault, text)
            .expect("fault live Text");
        function
            .append_block_parameter(cancel, text)
            .expect("cancel live Text");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::AwaitTasks {
                        state: 1,
                        mode,
                        tasks: tasks.into_boxed_slice(),
                        normal: ResultTarget::new(normal, [input_text]),
                        fault: UnwindTarget::new(fault, [input_text]),
                        cancel: BlockTarget::new(cancel, [input_text]),
                    },
                    root_origin,
                ),
            )
            .expect("await terminal children");
        function
            .terminate(
                fault,
                Terminator::new(TerminatorKind::ResumeFault, root_origin),
            )
            .expect("resume fault");
        function
            .terminate(
                cancel,
                Terminator::new(TerminatorKind::TaskCancelled, root_origin),
            )
            .expect("cancel");

        if matches!(prefix, TakePrefix::NonPrefix) {
            function
                .append_instruction(
                    normal,
                    InstructionKind::TextLength { text: live_text },
                    &[integer],
                    root_origin,
                )
                .expect("non-prefix instruction");
        }
        let take_order = match prefix {
            TakePrefix::Complete | TakePrefix::NonPrefix => {
                (0..terminal_tasks.len()).collect::<Vec<_>>()
            }
            TakePrefix::Missing => Vec::new(),
            TakePrefix::Partial => vec![0],
            TakePrefix::Reversed => (0..terminal_tasks.len()).rev().collect(),
            TakePrefix::Duplicate => (0..terminal_tasks.len())
                .chain(std::iter::once(0))
                .collect(),
        };
        for index in take_order {
            function
                .append_instruction(
                    normal,
                    InstructionKind::TaskOutcomeTake {
                        task: terminal_tasks[index],
                    },
                    &[outcome],
                    root_origin,
                )
                .expect("TaskOutcome");
        }
        if !matches!(prefix, TakePrefix::NonPrefix) {
            function
                .append_instruction(
                    normal,
                    InstructionKind::TextLength { text: live_text },
                    &[integer],
                    root_origin,
                )
                .expect("post-prefix live Text use");
        }
        let result = function
            .append_instruction(
                normal,
                InstructionKind::Constant(loom_codegen_ir::Constant::Unit),
                &[unit],
                root_origin,
            )
            .expect("Unit")[0];
        function
            .terminate(
                normal,
                Terminator::new(TerminatorKind::Return(result), root_origin),
            )
            .expect("return");
    }
    builder.finish()
}

#[test]
fn settled_and_race_terminal_handles_validate_and_dump_explicit_outcome_takes() {
    for (source, mode) in [(TakeSource::Settled, "settled"), (TakeSource::Race, "race")] {
        let fixture = canonical_program(source);
        let checked =
            check_program(fixture.program).expect("canonical terminal take must validate");
        let dump = dump_program(&checked);
        assert!(
            dump.contains(&format!("await_tasks {mode} state 1")),
            "{dump}"
        );
        assert!(dump.contains("task.outcome_take %"), "{dump}");
    }
}

#[test]
fn terminal_task_take_prefix_accepts_complete_settled_and_race_rows_before_live_work() {
    for mode in [AwaitMode::Settled, AwaitMode::Race] {
        check_program(terminal_prefix_program(mode, TakePrefix::Complete))
            .unwrap_or_else(|errors| panic!("valid {mode:?} take prefix failed: {errors:?}"));
    }
}

#[test]
fn settled_terminal_task_takes_reject_missing_partial_reversed_and_non_prefix_rows() {
    for prefix in [
        TakePrefix::Missing,
        TakePrefix::Partial,
        TakePrefix::Reversed,
        TakePrefix::NonPrefix,
    ] {
        let errors = validate_program(&terminal_prefix_program(AwaitMode::Settled, prefix))
            .expect_err("hostile settled prefix must fail");
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code() == ValidationCode::InvalidTaskOwnership
                    && error.message().contains("exact parameter order")
            }),
            "missing ordered-prefix diagnostic for {prefix:?}: {errors:?}"
        );
    }
}

#[test]
fn race_terminal_task_takes_reject_missing_and_non_prefix_rows() {
    for prefix in [TakePrefix::Missing, TakePrefix::NonPrefix] {
        let errors = validate_program(&terminal_prefix_program(AwaitMode::Race, prefix))
            .expect_err("hostile race prefix must fail");
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code() == ValidationCode::InvalidTaskOwnership
                    && error.message().contains("exact parameter order")
            }),
            "missing ordered-prefix diagnostic for {prefix:?}: {errors:?}"
        );
    }
}

#[test]
fn terminal_task_take_prefix_rejects_duplicate_capture_after_the_complete_prefix() {
    for mode in [AwaitMode::Settled, AwaitMode::Race] {
        let errors = validate_program(&terminal_prefix_program(mode, TakePrefix::Duplicate))
            .expect_err("duplicate terminal capture must fail");
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code() == ValidationCode::InvalidTaskOwnership
                    && error.message().contains("consumed more than once")
            }),
            "missing duplicate-capture diagnostic for {mode:?}: {errors:?}"
        );
    }
}

#[test]
fn outcome_take_is_an_explicit_safepoint_rooting_only_preexisting_live_values() {
    let fixture = canonical_program(TakeSource::Settled);
    let checked: CheckedProgram =
        check_program(fixture.program).expect("canonical terminal take must validate");
    let function = checked
        .as_program()
        .function(fixture.root)
        .expect("root function");
    let ValueDefinition::InstructionResult { instruction, .. } = function
        .value(fixture.take_result)
        .expect("take result")
        .definition()
    else {
        panic!("take result must name its instruction")
    };
    let plan = plan_managed_roots(&checked, fixture.root).expect("managed-root plan");
    let state = plan
        .state(ManagedSafepoint::Instruction(instruction))
        .expect("TaskOutcomeTake safepoint state");
    let state = usize::try_from(state).expect("state index");
    let row = &plan.bitmaps()[state * plan.bitmap_words()..][..plan.bitmap_words()];
    let rooted = plan
        .slots()
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            ((row[index / 64] & (1_u64 << (index % 64))) != 0).then_some(slot.value())
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(rooted, BTreeSet::from([fixture.live_text]));
    assert!(
        !plan
            .slots()
            .iter()
            .any(|slot| slot.value() == fixture.take_result),
        "the collecting instruction result is undefined at its own safepoint"
    );
}

#[test]
fn outcome_take_rejects_created_and_forwarded_task_handles() {
    for source in [TakeSource::Created, TakeSource::Forwarded] {
        let errors = validate_program(&canonical_program(source).program)
            .expect_err("a non-terminal Task handle must not reach outcome_take");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InvalidTaskOwnership
                && error.message().contains("leading normal block parameter")
        }));
    }
}

#[test]
fn outcome_take_rechecks_the_complete_canonical_nominal_shapes() {
    for (fault, outcome, message) in [
        (
            FaultShape::Canonical,
            OutcomeShape::WrongNominal,
            "TaskOutcome[T]",
        ),
        (
            FaultShape::WrongMessageType,
            OutcomeShape::Canonical,
            "TaskFault",
        ),
        (
            FaultShape::Canonical,
            OutcomeShape::WrongCompletedPayload,
            "Completed(T)",
        ),
    ] {
        let errors =
            validate_program(&outcome_program(fault, outcome, TakeSource::Settled, true).program)
                .expect_err("a forged TaskOutcome contract must fail");
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.message().contains(message)),
            "missing `{message}` diagnostic: {errors:?}"
        );
    }
}

#[test]
fn outcome_take_requires_collecting_executor_effects() {
    let errors = validate_program(
        &outcome_program(
            FaultShape::Canonical,
            OutcomeShape::Canonical,
            TakeSource::Settled,
            false,
        )
        .program,
    )
    .expect_err("TaskOutcomeTake cannot omit MAY_COLLECT");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::EffectMismatch && error.message().contains("MAY_COLLECT")
    }));
}
