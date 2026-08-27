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
    let tuple = builder
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
    let _ = tuple;
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
        Effects::MAY_SUSPEND.with_implications()
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
                        },
                        origin,
                    ),
                )
                .expect("await aliased Tasks");
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

#[expect(
    clippy::too_many_lines,
    reason = "the malformed-program matrix keeps coroutine plans and heterogeneous await edges together"
)]
fn await_tasks_program(duplicate: bool, swap_plan: bool) -> loom_codegen_ir::Program {
    let origin = Origin::synthetic(FunctionId(0));
    let int_origin = Origin::synthetic(FunctionId(1));
    let bool_origin = Origin::synthetic(FunctionId(2));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let task_int = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Int)))
        .expect("Task[Int]");
    let task_bool = builder
        .add_task_handle_type(Type::Task(Box::new(Type::Bool)))
        .expect("Task[Bool]");

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

    let root = builder
        .declare_function(
            origin,
            "await.root",
            Signature::new([], unit),
            Effects::MAY_SUSPEND.with_implications(),
        )
        .expect("root");
    {
        let mut function = builder.function(root).expect("root builder");
        let awaited = if swap_plan {
            vec![boolean, integer]
        } else {
            vec![integer, boolean]
        };
        function
            .set_coroutine_plan(CoroutinePlan::new(
                unit,
                [CoroutineSuspension::new(1, awaited, [])],
            ))
            .expect("root coroutine");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
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
        function
            .append_block_parameter(normal, integer)
            .expect("Int result");
        function
            .append_block_parameter(normal, boolean)
            .expect("Bool result");
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
                        normal: ResultTarget::new(normal, []),
                    },
                    origin,
                ),
            )
            .expect("await_tasks");
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
    }
    builder.finish()
}

#[test]
fn await_tasks_requires_unique_children_and_exact_planned_result_slots() {
    validate_program(&await_tasks_program(false, false))
        .expect("canonical heterogeneous await_tasks must validate");

    let duplicate = validate_program(&await_tasks_program(true, false))
        .expect_err("one child cannot be awaited twice");
    assert!(duplicate.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape
            && error.message().contains("more than once")
    }));

    let swapped = validate_program(&await_tasks_program(false, true))
        .expect_err("the suspension row cannot swap heterogeneous outputs");
    assert!(swapped.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidCoroutinePlan
            && error
                .message()
                .contains("does not match its child Task output")
    }));
}
