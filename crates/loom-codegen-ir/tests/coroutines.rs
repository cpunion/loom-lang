use loom_codegen_ir::{
    Constant, CoroutinePlan, CoroutineSuspension, Effects, InstructionKind, Origin, ProgramBuilder,
    Signature, TargetLayout, Terminator, TerminatorKind, ValidationCode, validate_program,
};
use loom_mir::{FunctionId, Type};

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
