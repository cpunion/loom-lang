use loom_codegen_ir::{
    BlockTarget, Constant, CoroutinePlan, CoroutineSuspension, Effects, FaultCode, FaultMetadata,
    InstructionKind, Origin, ProgramBuilder, Signature, TargetLayout, Terminator, TerminatorKind,
    ValidationCode, validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

#[test]
fn validator_accepts_a_fallible_coroutine_with_a_pointer_free_sum_result() {
    let origin = Origin::synthetic(FunctionId(0));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let result = builder
        .add_sum_type(
            Type::Nominal(TypeId(20), Vec::new()),
            &[Box::from([Type::Int]), Box::new([])],
        )
        .expect("pointer-free result sum");
    let root = builder
        .declare_function(
            origin,
            "coroutine.fallible_result",
            Signature::new([], result),
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
        let value = function
            .append_instruction(
                success,
                InstructionKind::Constant(Constant::Int(7)),
                &[integer],
                origin,
            )
            .expect("Int")[0];
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

    validate_program(&builder.finish()).expect("fallible pointer-free Result coroutine is valid");
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
