use loom_codegen_ir::{
    Constant, Effects, Origin, ProgramBuilder, Signature, TargetLayout, Terminator, TerminatorKind,
    ValidationCode, ValidationErrors,
};
use loom_mir::{FunctionId as MirFunctionId, Type};

fn unit_program_with_effects(effects: Effects) -> Result<(), ValidationErrors> {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let origin = Origin::synthetic(MirFunctionId(0));
    let function = builder
        .declare_function(origin, "effect.fixture", Signature::new([], unit), effects)
        .expect("declaration");
    {
        let mut function = builder.function(function).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_instruction(
                entry,
                loom_codegen_ir::InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit constant")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), origin),
            )
            .expect("return");
    }
    builder.finish_checked().map(|_| ())
}

#[test]
fn effect_implications_and_display_order_are_canonical() {
    assert!(Effects::MAY_FAULT.is_closed());
    assert_eq!(Effects::MAY_FAULT.to_string(), "may_fault");
    assert_eq!(
        Effects::MAY_COLLECT.with_implications(),
        Effects::NEEDS_RUNTIME.union(Effects::MAY_COLLECT)
    );
    assert_eq!(
        Effects::MAY_SUSPEND.with_implications(),
        Effects::NEEDS_RUNTIME
            .union(Effects::NEEDS_EXECUTOR)
            .union(Effects::MAY_SUSPEND)
    );
    let all = Effects::MAY_SUSPEND
        .union(Effects::MAY_COLLECT)
        .union(Effects::MAY_FAULT)
        .with_implications();
    assert_eq!(
        all.to_string(),
        "may_fault+needs_runtime+may_collect+needs_executor+may_suspend"
    );
}

#[test]
fn malicious_declarations_cannot_omit_implications_or_invent_capabilities() {
    for incomplete in [
        Effects::MAY_COLLECT,
        Effects::NEEDS_EXECUTOR,
        Effects::MAY_SUSPEND,
        Effects::MAY_SUSPEND.union(Effects::NEEDS_RUNTIME),
    ] {
        let errors = unit_program_with_effects(incomplete)
            .expect_err("an incomplete capability implication must be rejected");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::EffectImplication
                && error.message().contains("capability implications require")
        }));
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code() == ValidationCode::EffectMismatch)
        );
    }

    let closed_but_unjustified = Effects::MAY_FAULT
        .union(Effects::MAY_COLLECT)
        .union(Effects::MAY_SUSPEND)
        .with_implications();
    let errors = unit_program_with_effects(closed_but_unjustified)
        .expect_err("a body cannot invent closed runtime capabilities");
    assert!(
        errors
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::EffectMismatch)
    );
    assert!(
        !errors
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::EffectImplication)
    );
}
