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
            .set_coroutine_plan(CoroutinePlan::new(unit, [CoroutineSuspension::new(1, [])]))
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
            && error.message().contains("no matching await_task")
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
