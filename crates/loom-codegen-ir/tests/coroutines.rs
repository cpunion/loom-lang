use loom_codegen_ir::{
    BlockTarget, Constant, CoroutinePlan, CoroutineSuspension, Effects, FaultCode, FaultMetadata,
    InstructionKind, Origin, ProgramBuilder, ResultTarget, Signature, TargetLayout, Terminator,
    TerminatorKind, UnwindTarget, ValidationCode, validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

#[test]
fn validator_accepts_a_fallible_coroutine_with_a_managed_sum_result() {
    let origin = Origin::synthetic(FunctionId(0));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let text = builder
        .add_managed_text_type()
        .expect("register managed Text");
    let result = builder
        .add_sum_type(
            Type::Nominal(TypeId(20), Vec::new()),
            &[Box::from([Type::Text]), Box::new([])],
        )
        .expect("managed result sum");
    let root = builder
        .declare_function(
            origin,
            "coroutine.fallible_result",
            Signature::new([text], result),
            Effects::MAY_FAULT
                .union(Effects::NEEDS_EXECUTOR)
                .with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(result, []))
            .expect("fallible coroutine plan");
        let entry = function.create_block().expect("entry");
        let success = function.create_block().expect("success");
        let failure = function.create_block().expect("failure");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, text)
            .expect("Text parameter");
        let condition = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[boolean],
                origin,
            )
            .expect("Bool")[0];
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(success, []),
                        else_target: BlockTarget::new(failure, []),
                    },
                    origin,
                ),
            )
            .expect("branch");
        let result = function
            .append_instruction(
                success,
                InstructionKind::SumConstruct {
                    variant: 0,
                    payload: Box::from([value]),
                },
                &[result],
                origin,
            )
            .expect("Result")[0];
        function
            .terminate(
                success,
                Terminator::new(TerminatorKind::Return(result), origin),
            )
            .expect("return");
        function
            .terminate(
                failure,
                Terminator::new(
                    TerminatorKind::Fault {
                        metadata: FaultMetadata::runtime(FaultCode::IntegerOverflow),
                    },
                    origin,
                ),
            )
            .expect("fault");
    }

    validate_program(&builder.finish()).expect("fallible managed Result coroutine is valid");
}

#[test]
fn validator_rejects_a_coroutine_row_without_an_await_edge() {
    let origin = Origin::synthetic(FunctionId(0));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let root = builder
        .declare_function(
            origin,
            "coroutine.invalid",
            Signature::new([], unit),
            Effects::MAY_SUSPEND.with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(
                unit,
                [CoroutineSuspension::new(1, [unit], [])],
            ))
            .expect("unchecked coroutine plan");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), origin),
            )
            .expect("return");
    }

    let errors = validate_program(&builder.finish()).expect_err("missing await must fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidCoroutinePlan
            && error.message().contains("no matching await_tasks")
    }));
}

fn task_sleep_program(
    coroutine: bool,
    effects: Effects,
    wrong_result: bool,
    wrong_milliseconds: bool,
) -> loom_codegen_ir::Program {
    let origin = Origin::synthetic(FunctionId(0));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let task_unit = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Unit)))
        .expect("Task[Unit]");
    let task_bool = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Bool)))
        .expect("Task[Bool]");
    let root = builder
        .declare_function(origin, "coroutine.sleep", Signature::new([], unit), effects)
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        if coroutine {
            function
                .set_coroutine_plan(CoroutinePlan::new(unit, []))
                .expect("coroutine plan");
        }
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let fault = function.create_block().expect("fault");
        function.set_entry(entry).expect("set entry");
        let milliseconds = function
            .append_instruction(
                entry,
                if wrong_milliseconds {
                    InstructionKind::Constant(Constant::Bool(false))
                } else {
                    InstructionKind::Constant(Constant::Int(0))
                },
                &[if wrong_milliseconds { boolean } else { integer }],
                origin,
            )
            .expect("milliseconds")[0];
        function
            .append_block_parameter(normal, if wrong_result { task_bool } else { task_unit })
            .expect("sleep result");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::TaskSleep {
                        milliseconds,
                        normal: ResultTarget::new(normal, []),
                        fault: UnwindTarget::new(fault, []),
                    },
                    origin,
                ),
            )
            .expect("task.sleep");
        let result = function
            .append_instruction(
                normal,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit")[0];
        function
            .terminate(
                normal,
                Terminator::new(TerminatorKind::Return(result), origin),
            )
            .expect("return");
        function
            .terminate(fault, Terminator::new(TerminatorKind::ResumeFault, origin))
            .expect("resume fault");
    }
    builder.finish()
}

#[test]
fn task_sleep_requires_exact_coroutine_effects_and_task_unit_result() {
    let effects = Effects::MAY_FAULT
        .union(Effects::NEEDS_EXECUTOR)
        .with_implications();
    validate_program(&task_sleep_program(true, effects, false, false))
        .expect("canonical task.sleep must validate");

    let no_coroutine = validate_program(&task_sleep_program(false, effects, false, false))
        .expect_err("task.sleep outside a coroutine must fail");
    assert!(no_coroutine.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidCoroutinePlan
            && error
                .message()
                .contains("only valid in a checked coroutine")
    }));

    let wrong_effects = validate_program(&task_sleep_program(
        true,
        Effects::NEEDS_EXECUTOR.with_implications(),
        false,
        false,
    ))
    .expect_err("task.sleep cannot omit MAY_FAULT");
    assert!(wrong_effects.as_slice().iter().any(|error| {
        error.code() == ValidationCode::EffectMismatch && error.message().contains("MAY_FAULT")
    }));

    let missing_executor = validate_program(&task_sleep_program(
        true,
        Effects::MAY_FAULT.with_implications(),
        false,
        false,
    ))
    .expect_err("task.sleep cannot omit NEEDS_EXECUTOR");
    assert!(missing_executor.as_slice().iter().any(|error| {
        error.code() == ValidationCode::EffectMismatch && error.message().contains("NEEDS_EXECUTOR")
    }));

    let wrong_result = validate_program(&task_sleep_program(true, effects, true, false))
        .expect_err("task.sleep cannot forge Task[Bool] as Task[Unit]");
    assert!(
        wrong_result
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::BlockArgument)
    );

    let wrong_milliseconds = validate_program(&task_sleep_program(true, effects, false, true))
        .expect_err("task.sleep milliseconds must be Int");
    assert!(
        wrong_milliseconds
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::TypeMismatch)
    );
}

#[expect(
    clippy::fn_params_excessive_bools,
    clippy::too_many_lines,
    reason = "the malformed-program matrix keeps every exact join shape and independent validator defect visible in one builder"
)]
fn task_join_all_program(
    duplicate: bool,
    empty: bool,
    wrong_result: bool,
    coroutine: bool,
    effects: Effects,
) -> loom_codegen_ir::Program {
    let origin = Origin::synthetic(FunctionId(0));
    let int_origin = Origin::synthetic(FunctionId(1));
    let bool_origin = Origin::synthetic(FunctionId(2));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let _tuple = builder
        .add_tuple_type(&[Type::Int, Type::Bool])
        .expect("(Int, Bool)");
    let task_int = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Int)))
        .expect("Task[Int]");
    let task_bool = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Bool)))
        .expect("Task[Bool]");
    let task_tuple = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Tuple(vec![
            Type::Int,
            Type::Bool,
        ]))))
        .expect("Task[(Int, Bool)]");

    let int_child = builder
        .declare_function(
            int_origin,
            "join.int_child",
            Signature::new([], integer),
            Effects::NEEDS_EXECUTOR.with_implications(),
        )
        .expect("int child");
    {
        let mut function = builder.function(int_child).expect("int child builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(integer, []))
            .expect("int child coroutine");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(1)),
                &[integer],
                int_origin,
            )
            .expect("Int")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), int_origin),
            )
            .expect("return");
    }
    let bool_child = builder
        .declare_function(
            bool_origin,
            "join.bool_child",
            Signature::new([], boolean),
            Effects::NEEDS_EXECUTOR.with_implications(),
        )
        .expect("bool child");
    {
        let mut function = builder.function(bool_child).expect("bool child builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(boolean, []))
            .expect("bool child coroutine");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[boolean],
                bool_origin,
            )
            .expect("Bool")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), bool_origin),
            )
            .expect("return");
    }
    let root = builder
        .declare_function(origin, "join.root", Signature::new([], unit), effects)
        .expect("root");
    {
        let mut function = builder.function(root).expect("root builder");
        if coroutine {
            function
                .set_coroutine_plan(CoroutinePlan::new(unit, []))
                .expect("root coroutine");
        }
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        let first = function
            .append_instruction(
                entry,
                InstructionKind::TaskCreate {
                    coroutine: int_child,
                    arguments: Box::new([]),
                },
                &[task_int],
                origin,
            )
            .expect("Task[Int]")[0];
        let second = function
            .append_instruction(
                entry,
                InstructionKind::TaskCreate {
                    coroutine: bool_child,
                    arguments: Box::new([]),
                },
                &[task_bool],
                origin,
            )
            .expect("Task[Bool]")[0];
        let tasks = if empty {
            Vec::new()
        } else if duplicate {
            vec![first, first]
        } else {
            vec![first, second]
        };
        function
            .append_instruction(
                entry,
                InstructionKind::TaskJoinAll {
                    tasks: tasks.into_boxed_slice(),
                },
                &[if wrong_result { task_int } else { task_tuple }],
                origin,
            )
            .expect("join result");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin),
            )
            .expect("return");
    }
    builder.finish()
}

#[test]
fn task_join_all_requires_unique_exact_children_result_and_executor_context() {
    let effects = Effects::NEEDS_EXECUTOR.with_implications();
    validate_program(&task_join_all_program(false, false, false, true, effects))
        .expect("canonical heterogeneous task.join_all must validate");

    let duplicate = validate_program(&task_join_all_program(true, false, false, true, effects))
        .expect_err("one child cannot be consumed twice");
    assert!(duplicate.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape
            && error.message().contains("more than once")
    }));

    let empty = validate_program(&task_join_all_program(false, true, false, true, effects))
        .expect_err("an empty static join must fail");
    assert!(empty.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape && error.message().contains("at least one")
    }));

    let wrong_result = validate_program(&task_join_all_program(false, false, true, true, effects))
        .expect_err("the composite output must be the exact heterogeneous tuple");
    assert!(
        wrong_result
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::TypeMismatch)
    );

    let no_coroutine =
        validate_program(&task_join_all_program(false, false, false, false, effects))
            .expect_err("a sync function cannot gain a hidden executor context");
    assert!(no_coroutine.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidCoroutinePlan
            && error.message().contains("active typed-coroutine")
    }));

    let no_executor = validate_program(&task_join_all_program(
        false,
        false,
        false,
        true,
        Effects::NONE,
    ))
    .expect_err("task.join_all requires executor effects");
    assert!(no_executor.as_slice().iter().any(|error| {
        error.code() == ValidationCode::EffectMismatch && error.message().contains("NEEDS_EXECUTOR")
    }));
}

#[expect(
    clippy::too_many_lines,
    reason = "one raw LCIR builder exposes edge aliasing and cross-site Task consumption without relying on source ownership checks"
)]
fn invalid_task_ownership_program(
    await_children: bool,
    reuse_children: bool,
) -> loom_codegen_ir::Program {
    let origin = Origin::synthetic(FunctionId(0));
    let child_origin = Origin::synthetic(FunctionId(1));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let tuple = builder
        .add_tuple_type(&[Type::Int, Type::Int])
        .expect("(Int, Int)");
    let task_int = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Int)))
        .expect("Task[Int]");
    let task_tuple = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Tuple(vec![
            Type::Int,
            Type::Int,
        ]))))
        .expect("Task[(Int, Int)]");

    let child = builder
        .declare_function(
            child_origin,
            "aliased.child",
            Signature::new([], integer),
            Effects::NEEDS_EXECUTOR.with_implications(),
        )
        .expect("child");
    {
        let mut function = builder.function(child).expect("child builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(integer, []))
            .expect("child coroutine");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(1)),
                &[integer],
                child_origin,
            )
            .expect("Int")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), child_origin),
            )
            .expect("return");
    }

    let effects = if await_children {
        Effects::MAY_FAULT
            .union(Effects::MAY_SUSPEND)
            .with_implications()
    } else {
        Effects::NEEDS_EXECUTOR.with_implications()
    };
    let root = builder
        .declare_function(origin, "aliased.root", Signature::new([], unit), effects)
        .expect("root");
    {
        let mut function = builder.function(root).expect("root builder");
        let suspensions =
            await_children.then(|| vec![CoroutineSuspension::new(1, [integer, integer], [])]);
        function
            .set_coroutine_plan(CoroutinePlan::new(unit, suspensions.unwrap_or_default()))
            .expect("root coroutine");
        let entry = function.create_block().expect("entry");
        let forwarded = function.create_block().expect("forwarded");
        function.set_entry(entry).expect("entry");
        let task = function
            .append_instruction(
                entry,
                InstructionKind::TaskCreate {
                    coroutine: child,
                    arguments: Box::new([]),
                },
                &[task_int],
                origin,
            )
            .expect("Task[Int]")[0];
        let second_task = if reuse_children {
            function
                .append_instruction(
                    entry,
                    InstructionKind::TaskCreate {
                        coroutine: child,
                        arguments: Box::new([]),
                    },
                    &[task_int],
                    origin,
                )
                .expect("second Task[Int]")[0]
        } else {
            task
        };
        let first = function
            .append_block_parameter(forwarded, task_int)
            .expect("first Task[Int]");
        let second = function
            .append_block_parameter(forwarded, task_int)
            .expect("second Task[Int]");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Jump(BlockTarget::new(forwarded, [task, second_task])),
                    origin,
                ),
            )
            .expect("Task forwarding edge");

        let return_block = if await_children {
            let normal = function.create_block().expect("normal");
            let fault = function.create_block().expect("fault");
            let cancel = function.create_block().expect("cancel");
            function
                .append_block_parameter(normal, integer)
                .expect("first Int result");
            function
                .append_block_parameter(normal, integer)
                .expect("second Int result");
            function
                .terminate(
                    forwarded,
                    Terminator::new(
                        TerminatorKind::AwaitTasks {
                            state: 1,
                            tasks: Box::from([first, second]),
                            normal: ResultTarget::new(normal, []),
                            fault: UnwindTarget::new(fault, []),
                            cancel: BlockTarget::new(cancel, []),
                        },
                        origin,
                    ),
                )
                .expect("await aliased Tasks");
            function
                .terminate(fault, Terminator::new(TerminatorKind::ResumeFault, origin))
                .expect("propagate child fault");
            function
                .terminate(
                    cancel,
                    Terminator::new(TerminatorKind::TaskCancelled, origin),
                )
                .expect("propagate child cancellation");
            normal
        } else {
            function
                .append_instruction(
                    forwarded,
                    InstructionKind::TaskJoinAll {
                        tasks: Box::from([first, second]),
                    },
                    &[task_tuple],
                    origin,
                )
                .expect("join aliased Tasks");
            if reuse_children {
                function
                    .append_instruction(
                        forwarded,
                        InstructionKind::TaskJoinAll {
                            tasks: Box::from([first, second]),
                        },
                        &[task_tuple],
                        origin,
                    )
                    .expect("reuse joined Tasks");
            }
            forwarded
        };
        let result = function
            .append_instruction(
                return_block,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit")[0];
        function
            .terminate(
                return_block,
                Terminator::new(TerminatorKind::Return(result), origin),
            )
            .expect("return");
    }
    let _ = tuple;
    builder.finish()
}

#[test]
fn task_ownership_rejects_aliases_hidden_behind_distinct_block_parameters() {
    for await_children in [false, true] {
        let errors = validate_program(&invalid_task_ownership_program(await_children, false))
            .expect_err("one Task cannot be forwarded into two apparent child handles");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InvalidTaskOwnership
                && error.message().contains("consumed more than once")
        }));
    }

    let reused = validate_program(&invalid_task_ownership_program(false, true))
        .expect_err("consumed Task children cannot enter a later join");
    assert!(reused.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidTaskOwnership
            && error.message().contains("consumed more than once")
    }));
}

#[derive(Clone, Copy)]
enum TaskOwnershipCfgCase {
    ExclusiveBranch,
    PartialBranchConsumption,
    LoopCarried,
    InvokeReuse,
}

#[expect(
    clippy::too_many_lines,
    reason = "one raw LCIR fixture keeps the ownership-only CFG variations directly comparable"
)]
fn task_ownership_cfg_program(case: TaskOwnershipCfgCase) -> loom_codegen_ir::Program {
    let origin = Origin::synthetic(FunctionId(0));
    let sink_origin = Origin::synthetic(FunctionId(1));
    let fallible_origin = Origin::synthetic(FunctionId(2));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let task_int = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Int)))
        .expect("Task[Int]");

    let sink = builder
        .declare_function(
            sink_origin,
            "ownership.sink",
            Signature::new([task_int], task_int),
            Effects::NONE,
        )
        .expect("sink");
    {
        let mut function = builder.function(sink).expect("sink builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        let task = function
            .append_block_parameter(entry, task_int)
            .expect("owned Task[Int]");
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(task), sink_origin),
            )
            .expect("return");
    }

    let fallible_sink = builder
        .declare_function(
            fallible_origin,
            "ownership.fallible_sink",
            Signature::new([task_int], task_int),
            Effects::MAY_FAULT.with_implications(),
        )
        .expect("fallible sink");
    {
        let mut function = builder
            .function(fallible_sink)
            .expect("fallible sink builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        function
            .append_block_parameter(entry, task_int)
            .expect("owned Task[Int]");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Fault {
                        metadata: FaultMetadata::runtime(FaultCode::IntegerOverflow),
                    },
                    fallible_origin,
                ),
            )
            .expect("fault");
    }

    let effects = if matches!(case, TaskOwnershipCfgCase::InvokeReuse) {
        Effects::MAY_FAULT.with_implications()
    } else {
        Effects::NONE
    };
    let root = builder
        .declare_function(
            origin,
            "ownership.cfg",
            Signature::new([task_int, boolean], task_int),
            effects,
        )
        .expect("root");
    {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        let task = function
            .append_block_parameter(entry, task_int)
            .expect("Task[Int]");
        let condition = function
            .append_block_parameter(entry, boolean)
            .expect("condition");

        match case {
            TaskOwnershipCfgCase::ExclusiveBranch => {
                let then_block = function.create_block().expect("then");
                let else_block = function.create_block().expect("else");
                let then_task = function
                    .append_block_parameter(then_block, task_int)
                    .expect("then Task[Int]");
                let else_task = function
                    .append_block_parameter(else_block, task_int)
                    .expect("else Task[Int]");
                function
                    .terminate(
                        entry,
                        Terminator::new(
                            TerminatorKind::Branch {
                                condition,
                                then_target: BlockTarget::new(then_block, [task]),
                                else_target: BlockTarget::new(else_block, [task]),
                            },
                            origin,
                        ),
                    )
                    .expect("branch");
                function
                    .terminate(
                        then_block,
                        Terminator::new(TerminatorKind::Return(then_task), origin),
                    )
                    .expect("then return");
                function
                    .terminate(
                        else_block,
                        Terminator::new(TerminatorKind::Return(else_task), origin),
                    )
                    .expect("else return");
            }
            TaskOwnershipCfgCase::PartialBranchConsumption => {
                let consumed = function.create_block().expect("consumed");
                let skipped = function.create_block().expect("skipped");
                let merge = function.create_block().expect("merge");
                function
                    .terminate(
                        entry,
                        Terminator::new(
                            TerminatorKind::Branch {
                                condition,
                                then_target: BlockTarget::new(consumed, []),
                                else_target: BlockTarget::new(skipped, []),
                            },
                            origin,
                        ),
                    )
                    .expect("branch");
                function
                    .append_instruction(
                        consumed,
                        InstructionKind::DirectCall {
                            callee: sink,
                            arguments: Box::from([task]),
                        },
                        &[task_int],
                        origin,
                    )
                    .expect("consume Task");
                function
                    .terminate(
                        consumed,
                        Terminator::new(TerminatorKind::Jump(BlockTarget::new(merge, [])), origin),
                    )
                    .expect("consumed merge");
                function
                    .terminate(
                        skipped,
                        Terminator::new(TerminatorKind::Jump(BlockTarget::new(merge, [])), origin),
                    )
                    .expect("skipped merge");
                function
                    .terminate(merge, Terminator::new(TerminatorKind::Return(task), origin))
                    .expect("reuse after merge");
            }
            TaskOwnershipCfgCase::LoopCarried => {
                let header = function.create_block().expect("header");
                let exit = function.create_block().expect("exit");
                let carried = function
                    .append_block_parameter(header, task_int)
                    .expect("carried Task[Int]");
                let exit_task = function
                    .append_block_parameter(exit, task_int)
                    .expect("exit Task[Int]");
                function
                    .terminate(
                        entry,
                        Terminator::new(
                            TerminatorKind::Jump(BlockTarget::new(header, [task])),
                            origin,
                        ),
                    )
                    .expect("enter loop");
                function
                    .terminate(
                        header,
                        Terminator::new(
                            TerminatorKind::Branch {
                                condition,
                                then_target: BlockTarget::new(header, [carried]),
                                else_target: BlockTarget::new(exit, [carried]),
                            },
                            origin,
                        ),
                    )
                    .expect("loop or exit");
                function
                    .terminate(
                        exit,
                        Terminator::new(TerminatorKind::Return(exit_task), origin),
                    )
                    .expect("return carried Task");
            }
            TaskOwnershipCfgCase::InvokeReuse => {
                let normal = function.create_block().expect("normal");
                let unwind = function.create_block().expect("unwind");
                function
                    .append_block_parameter(normal, task_int)
                    .expect("invoke result");
                function
                    .terminate(
                        entry,
                        Terminator::new(
                            TerminatorKind::Invoke {
                                callee: fallible_sink,
                                arguments: Box::from([task]),
                                normal: ResultTarget::new(normal, []),
                                unwind: UnwindTarget::new(unwind, []),
                            },
                            origin,
                        ),
                    )
                    .expect("invoke");
                function
                    .terminate(
                        normal,
                        Terminator::new(TerminatorKind::Return(task), origin),
                    )
                    .expect("reuse invoke argument");
                function
                    .terminate(unwind, Terminator::new(TerminatorKind::ResumeFault, origin))
                    .expect("resume fault");
            }
        }
    }
    builder.finish()
}

#[test]
fn task_ownership_allows_mutually_exclusive_branch_moves() {
    validate_program(&task_ownership_cfg_program(
        TaskOwnershipCfgCase::ExclusiveBranch,
    ))
    .expect("one Task may move along either mutually exclusive branch");
}

#[test]
fn task_ownership_rejects_consumption_on_only_one_incoming_path() {
    let errors = validate_program(&task_ownership_cfg_program(
        TaskOwnershipCfgCase::PartialBranchConsumption,
    ))
    .expect_err("a merged Task must be available on every incoming path");
    assert!(
        errors
            .as_slice()
            .iter()
            .all(|error| error.code() == ValidationCode::InvalidTaskOwnership)
    );
}

#[test]
fn task_ownership_allows_loop_carried_moves_before_exit() {
    validate_program(&task_ownership_cfg_program(
        TaskOwnershipCfgCase::LoopCarried,
    ))
    .expect("a Task may be rebound through a loop parameter before its exit move");
}

#[test]
fn task_ownership_rejects_reusing_an_invoke_argument() {
    let errors = validate_program(&task_ownership_cfg_program(
        TaskOwnershipCfgCase::InvokeReuse,
    ))
    .expect_err("Invoke transfers its Task arguments on both continuations");
    assert!(
        errors
            .as_slice()
            .iter()
            .all(|error| error.code() == ValidationCode::InvalidTaskOwnership)
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "the malformed-program matrix keeps coroutine plans and heterogeneous await edges together"
)]
fn await_tasks_program(
    duplicate: bool,
    swap_plan: bool,
    exit_case: AwaitExitCase,
) -> loom_codegen_ir::Program {
    let origin = Origin::synthetic(FunctionId(0));
    let int_origin = Origin::synthetic(FunctionId(1));
    let bool_origin = Origin::synthetic(FunctionId(2));
    let fallible_origin = Origin::synthetic(FunctionId(3));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let _tuple = builder
        .add_tuple_type(&[Type::Int, Type::Bool])
        .expect("(Int, Bool)");
    let task_int = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Int)))
        .expect("Task[Int]");
    let task_bool = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Bool)))
        .expect("Task[Bool]");
    let task_unit = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Unit)))
        .expect("Task[Unit]");
    let task_tuple = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Tuple(vec![
            Type::Int,
            Type::Bool,
        ]))))
        .expect("Task[(Int, Bool)]");

    let int_child = builder
        .declare_function(
            int_origin,
            "await.int_child",
            Signature::new([], integer),
            Effects::NEEDS_EXECUTOR.with_implications(),
        )
        .expect("int child");
    {
        let mut function = builder.function(int_child).expect("int child builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(integer, []))
            .expect("int child coroutine");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(1)),
                &[integer],
                int_origin,
            )
            .expect("Int")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), int_origin),
            )
            .expect("return");
    }
    let bool_child = builder
        .declare_function(
            bool_origin,
            "await.bool_child",
            Signature::new([], boolean),
            Effects::NEEDS_EXECUTOR.with_implications(),
        )
        .expect("bool child");
    {
        let mut function = builder.function(bool_child).expect("bool child builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(boolean, []))
            .expect("bool child coroutine");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[boolean],
                bool_origin,
            )
            .expect("Bool")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), bool_origin),
            )
            .expect("return");
    }
    let fallible_child = builder
        .declare_function(
            fallible_origin,
            "await.fallible_child",
            Signature::new([], unit),
            Effects::MAY_FAULT
                .union(Effects::NEEDS_EXECUTOR)
                .with_implications(),
        )
        .expect("fallible child");
    {
        let mut function = builder
            .function(fallible_child)
            .expect("fallible child builder");
        function
            .set_coroutine_plan(CoroutinePlan::new(unit, []))
            .expect("fallible child coroutine");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Fault {
                        metadata: FaultMetadata::runtime(FaultCode::IntegerOverflow),
                    },
                    fallible_origin,
                ),
            )
            .expect("fault");
    }

    let root = builder
        .declare_function(
            origin,
            "await.root",
            Signature::new([], unit),
            Effects::MAY_FAULT
                .union(Effects::MAY_SUSPEND)
                .with_implications(),
        )
        .expect("root");
    {
        let mut function = builder.function(root).expect("root builder");
        let awaited = if swap_plan {
            vec![boolean, integer]
        } else {
            vec![integer, boolean]
        };
        let mut suspensions = vec![CoroutineSuspension::new(1, awaited, [integer])];
        if matches!(
            exit_case,
            AwaitExitCase::CancelSuspends | AwaitExitCase::FaultSuspends
        ) {
            suspensions.push(CoroutineSuspension::new(2, [integer], []));
        }
        function
            .set_coroutine_plan(CoroutinePlan::new(unit, suspensions))
            .expect("root coroutine");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let fault = function.create_block().expect("fault");
        let cancel = function.create_block().expect("cancel");
        function.set_entry(entry).expect("entry");
        let first = function
            .append_instruction(
                entry,
                InstructionKind::TaskCreate {
                    coroutine: int_child,
                    arguments: Box::new([]),
                },
                &[task_int],
                origin,
            )
            .expect("Task[Int]")[0];
        let second = function
            .append_instruction(
                entry,
                InstructionKind::TaskCreate {
                    coroutine: bool_child,
                    arguments: Box::new([]),
                },
                &[task_bool],
                origin,
            )
            .expect("Task[Bool]")[0];
        let carried = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(10)),
                &[integer],
                origin,
            )
            .expect("live Int")[0];
        let alternate = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(20)),
                &[integer],
                origin,
            )
            .expect("alternate Int")[0];
        function
            .append_block_parameter(normal, integer)
            .expect("Int result");
        function
            .append_block_parameter(normal, boolean)
            .expect("Bool result");
        function
            .append_block_parameter(normal, integer)
            .expect("normal live Int");
        function
            .append_block_parameter(fault, integer)
            .expect("fault live Int");
        let cancel_live = function
            .append_block_parameter(cancel, integer)
            .expect("cancel live Int");
        let fault_argument = if matches!(exit_case, AwaitExitCase::MismatchedFault) {
            alternate
        } else {
            carried
        };
        let cancel_argument = if matches!(exit_case, AwaitExitCase::MismatchedCancel) {
            alternate
        } else {
            carried
        };
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::AwaitTasks {
                        state: 1,
                        tasks: if duplicate {
                            Box::from([first, first])
                        } else {
                            Box::from([first, second])
                        },
                        normal: ResultTarget::new(normal, [carried]),
                        fault: UnwindTarget::new(fault, [fault_argument]),
                        cancel: BlockTarget::new(cancel, [cancel_argument]),
                    },
                    origin,
                ),
            )
            .expect("await_tasks");
        if matches!(exit_case, AwaitExitCase::FaultSuspends) {
            let resumed = function.create_block().expect("fault cleanup await normal");
            let cleanup_fault = function.create_block().expect("fault cleanup await fault");
            let cleanup_cancel = function
                .create_block()
                .expect("fault cleanup await cancellation");
            let cleanup_task = function
                .append_instruction(
                    fault,
                    InstructionKind::TaskCreate {
                        coroutine: int_child,
                        arguments: Box::new([]),
                    },
                    &[task_int],
                    origin,
                )
                .expect("fault cleanup Task[Int]")[0];
            function
                .append_block_parameter(resumed, integer)
                .expect("fault cleanup awaited Int");
            function
                .terminate(
                    fault,
                    Terminator::new(
                        TerminatorKind::AwaitTasks {
                            state: 2,
                            tasks: Box::from([cleanup_task]),
                            normal: ResultTarget::new(resumed, []),
                            fault: UnwindTarget::new(cleanup_fault, []),
                            cancel: BlockTarget::new(cleanup_cancel, []),
                        },
                        origin,
                    ),
                )
                .expect("invalid fault-cleanup suspension");
            function
                .terminate(
                    resumed,
                    Terminator::new(TerminatorKind::ResumeFault, origin),
                )
                .expect("resume original fault after invalid suspension");
            function
                .terminate(
                    cleanup_fault,
                    Terminator::new(TerminatorKind::ResumeFault, origin),
                )
                .expect("propagate cleanup child fault");
            function
                .terminate(
                    cleanup_cancel,
                    Terminator::new(TerminatorKind::TaskCancelled, origin),
                )
                .expect("propagate cleanup child cancellation");
        } else {
            function
                .terminate(fault, Terminator::new(TerminatorKind::ResumeFault, origin))
                .expect("propagate child fault");
        }
        if matches!(exit_case, AwaitExitCase::CancelSuspends) {
            let resumed = function.create_block().expect("cancel await normal");
            let cleanup_fault = function.create_block().expect("cancel await fault");
            let cleanup_cancel = function.create_block().expect("cancel await cancellation");
            let cleanup_task = function
                .append_instruction(
                    cancel,
                    InstructionKind::TaskCreate {
                        coroutine: int_child,
                        arguments: Box::new([]),
                    },
                    &[task_int],
                    origin,
                )
                .expect("cleanup Task[Int]")[0];
            function
                .append_block_parameter(resumed, integer)
                .expect("cleanup awaited Int");
            function
                .terminate(
                    cancel,
                    Terminator::new(
                        TerminatorKind::AwaitTasks {
                            state: 2,
                            tasks: Box::from([cleanup_task]),
                            normal: ResultTarget::new(resumed, []),
                            fault: UnwindTarget::new(cleanup_fault, []),
                            cancel: BlockTarget::new(cleanup_cancel, []),
                        },
                        origin,
                    ),
                )
                .expect("invalid cancellation suspension");
            let result = function
                .append_instruction(
                    resumed,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit],
                    origin,
                )
                .expect("resumed Unit")[0];
            function
                .terminate(
                    resumed,
                    Terminator::new(TerminatorKind::Return(result), origin),
                )
                .expect("invalid resumed return");
            function
                .terminate(
                    cleanup_fault,
                    Terminator::new(TerminatorKind::ResumeFault, origin),
                )
                .expect("cleanup child fault");
            function
                .terminate(
                    cleanup_cancel,
                    Terminator::new(TerminatorKind::TaskCancelled, origin),
                )
                .expect("cleanup child cancellation");
        } else if matches!(exit_case, AwaitExitCase::CancelMutatesTopology) {
            let third = function
                .append_instruction(
                    cancel,
                    InstructionKind::TaskCreate {
                        coroutine: int_child,
                        arguments: Box::new([]),
                    },
                    &[task_int],
                    origin,
                )
                .expect("cancellation Task[Int]")[0];
            let fourth = function
                .append_instruction(
                    cancel,
                    InstructionKind::TaskCreate {
                        coroutine: bool_child,
                        arguments: Box::new([]),
                    },
                    &[task_bool],
                    origin,
                )
                .expect("cancellation Task[Bool]")[0];
            function
                .append_instruction(
                    cancel,
                    InstructionKind::TaskJoinAll {
                        tasks: Box::from([third, fourth]),
                    },
                    &[task_tuple],
                    origin,
                )
                .expect("cancellation Task[(Int, Bool)]");
            function
                .terminate(
                    cancel,
                    Terminator::new(TerminatorKind::TaskCancelled, origin),
                )
                .expect("cancel after invalid topology mutation");
        } else if matches!(exit_case, AwaitExitCase::CancelSleeps) {
            let sleep_normal = function.create_block().expect("cancel sleep normal");
            let sleep_fault = function.create_block().expect("cancel sleep fault");
            function
                .append_block_parameter(sleep_normal, task_unit)
                .expect("cancel sleep Task[Unit]");
            function
                .terminate(
                    cancel,
                    Terminator::new(
                        TerminatorKind::TaskSleep {
                            milliseconds: cancel_live,
                            normal: ResultTarget::new(sleep_normal, []),
                            fault: UnwindTarget::new(sleep_fault, []),
                        },
                        origin,
                    ),
                )
                .expect("invalid cancellation Task.sleep");
            function
                .terminate(
                    sleep_normal,
                    Terminator::new(TerminatorKind::TaskCancelled, origin),
                )
                .expect("cancel after invalid Task.sleep");
            function
                .terminate(
                    sleep_fault,
                    Terminator::new(TerminatorKind::ResumeFault, origin),
                )
                .expect("propagate invalid Task.sleep fault");
        } else if matches!(exit_case, AwaitExitCase::CancelDirectCallsExecutor) {
            function
                .append_instruction(
                    cancel,
                    InstructionKind::DirectCall {
                        callee: int_child,
                        arguments: Box::new([]),
                    },
                    &[integer],
                    origin,
                )
                .expect("invalid executor-dependent call");
            function
                .terminate(
                    cancel,
                    Terminator::new(TerminatorKind::TaskCancelled, origin),
                )
                .expect("cancel after invalid executor-dependent call");
        } else if matches!(exit_case, AwaitExitCase::CancelInvokesExecutor) {
            let invoke_normal = function.create_block().expect("cancel invoke normal");
            let invoke_unwind = function.create_block().expect("cancel invoke unwind");
            function
                .append_block_parameter(invoke_normal, unit)
                .expect("cancel invoke Unit");
            function
                .terminate(
                    cancel,
                    Terminator::new(
                        TerminatorKind::Invoke {
                            callee: fallible_child,
                            arguments: Box::new([]),
                            normal: ResultTarget::new(invoke_normal, []),
                            unwind: UnwindTarget::new(invoke_unwind, []),
                        },
                        origin,
                    ),
                )
                .expect("invalid executor-dependent invoke");
            function
                .terminate(
                    invoke_normal,
                    Terminator::new(TerminatorKind::TaskCancelled, origin),
                )
                .expect("cancel after invalid executor-dependent invoke");
            function
                .terminate(
                    invoke_unwind,
                    Terminator::new(TerminatorKind::ResumeFault, origin),
                )
                .expect("propagate invalid executor-dependent invoke fault");
        } else if matches!(exit_case, AwaitExitCase::CancelReturns) {
            let result = function
                .append_instruction(
                    cancel,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit],
                    origin,
                )
                .expect("cancel Unit")[0];
            function
                .terminate(
                    cancel,
                    Terminator::new(TerminatorKind::Return(result), origin),
                )
                .expect("invalid normal return from cancellation");
        } else {
            function
                .terminate(
                    cancel,
                    Terminator::new(TerminatorKind::TaskCancelled, origin),
                )
                .expect("propagate child cancellation");
        }
        let result = function
            .append_instruction(
                normal,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit")[0];
        function
            .terminate(
                normal,
                Terminator::new(
                    if matches!(exit_case, AwaitExitCase::NormalCancels) {
                        TerminatorKind::TaskCancelled
                    } else {
                        TerminatorKind::Return(result)
                    },
                    origin,
                ),
            )
            .expect("normal terminal");
    }
    builder.finish()
}

#[derive(Clone, Copy)]
enum AwaitExitCase {
    Canonical,
    MismatchedFault,
    MismatchedCancel,
    CancelReturns,
    NormalCancels,
    CancelSuspends,
    CancelMutatesTopology,
    CancelSleeps,
    CancelDirectCallsExecutor,
    CancelInvokesExecutor,
    FaultSuspends,
}

#[test]
fn await_tasks_requires_unique_children_and_exact_planned_result_slots() {
    validate_program(&await_tasks_program(false, false, AwaitExitCase::Canonical))
        .expect("canonical heterogeneous await_tasks must validate");

    let duplicate = validate_program(&await_tasks_program(true, false, AwaitExitCase::Canonical))
        .expect_err("one child cannot be awaited twice");
    assert!(duplicate.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape
            && error.message().contains("more than once")
    }));

    let swapped = validate_program(&await_tasks_program(false, true, AwaitExitCase::Canonical))
        .expect_err("the suspension row cannot swap heterogeneous outputs");
    assert!(swapped.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidCoroutinePlan
            && error
                .message()
                .contains("does not match its child Task output")
    }));
}

#[test]
fn await_tasks_requires_exact_live_rows_on_fault_and_cancel_edges() {
    for exit_case in [
        AwaitExitCase::MismatchedFault,
        AwaitExitCase::MismatchedCancel,
    ] {
        let errors = validate_program(&await_tasks_program(false, false, exit_case))
            .expect_err("every suspension exit must forward the identical live SSA row");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InvalidCoroutinePlan
                && error.message().contains("same exact live-value row")
        }));
    }
}

#[test]
fn cancellation_cannot_be_forged_or_laundered_into_a_normal_return() {
    let forged = validate_program(&await_tasks_program(
        false,
        false,
        AwaitExitCase::NormalCancels,
    ))
    .expect_err("an ordinary continuation cannot report cancellation");
    assert!(forged.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidCoroutinePlan
            && error.message().contains("requires an active cancellation")
    }));

    let laundered = validate_program(&await_tasks_program(
        false,
        false,
        AwaitExitCase::CancelReturns,
    ))
    .expect_err("a cancellation continuation cannot return normally");
    assert!(laundered.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidCoroutinePlan
            && error.message().contains("cannot return normally")
    }));
}

#[test]
fn cancellation_cleanup_cannot_suspend_again() {
    let errors = validate_program(&await_tasks_program(
        false,
        false,
        AwaitExitCase::CancelSuspends,
    ))
    .expect_err("a cancellation cleanup continuation cannot await another Task");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidCoroutinePlan
            && error.message()
                == "cancellation cleanup cannot create or await a Task; it must remain scheduler-topology neutral"
    }));
}

#[test]
fn cancellation_cleanup_cannot_call_or_invoke_executor_dependent_functions() {
    for (exit_case, diagnostic) in [
        (
            AwaitExitCase::CancelDirectCallsExecutor,
            "cancellation cleanup cannot call an executor-dependent function; it must remain scheduler-topology neutral",
        ),
        (
            AwaitExitCase::CancelInvokesExecutor,
            "cancellation cleanup cannot invoke an executor-dependent function; it must remain scheduler-topology neutral",
        ),
    ] {
        let errors = validate_program(&await_tasks_program(false, false, exit_case))
            .expect_err("cancellation cleanup cannot enter executor-dependent code");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InvalidCoroutinePlan && error.message() == diagnostic
        }));
    }
}

#[test]
fn cancellation_cleanup_cannot_create_sleep_or_aggregate_tasks() {
    let topology = validate_program(&await_tasks_program(
        false,
        false,
        AwaitExitCase::CancelMutatesTopology,
    ))
    .expect_err("cancellation cleanup must not create or aggregate Tasks");
    for operation in ["create a Task", "construct a Task join"] {
        assert!(topology.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InvalidCoroutinePlan
                && error.message().contains(operation)
        }));
    }

    let sleep = validate_program(&await_tasks_program(
        false,
        false,
        AwaitExitCase::CancelSleeps,
    ))
    .expect_err("cancellation cleanup must not create a timer Task");
    assert!(sleep.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidCoroutinePlan
            && error.message().contains("cannot create or await a Task")
    }));
}

#[test]
fn source_fault_cleanup_cannot_suspend_again() {
    let errors = validate_program(&await_tasks_program(
        false,
        false,
        AwaitExitCase::FaultSuspends,
    ))
    .expect_err("source-fault cleanup must finish without another await");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::FaultState
            && error
                .message()
                .contains("source-fault cleanup cannot suspend again")
    }));
}
