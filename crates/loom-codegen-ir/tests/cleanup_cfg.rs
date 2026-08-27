use loom_codegen_ir::{
    BlockTarget, Constant, ContractFaultMetadata, Effects, InstructionKind, Origin, Program,
    ProgramBuilder, ResourceKind, ResultTarget, Signature, TargetLayout, Terminator,
    TerminatorKind, UnwindTarget, ValidationCode, dump_program,
};
use loom_mir::{FunctionId, Type, TypeId};

fn origin(function: u32) -> Origin {
    Origin::synthetic(FunctionId(function))
}

fn resource_program(fields: &[Type], effects: Effects) -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = program.type_id(&Type::Unit).expect("Unit");
    let resource = program
        .add_pod_record_type(Type::Nominal(TypeId(90), Vec::new()), fields)
        .expect("resource product");
    let function = program
        .declare_function(
            origin(0),
            "cleanup.resource",
            Signature::new([resource], unit),
            effects,
        )
        .expect("declare resource cleanup");
    {
        let mut function = program.function(function).expect("function builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let fault = function.create_block().expect("fault");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, resource)
            .expect("resource");
        let returned = function
            .append_block_parameter(normal, unit)
            .expect("Unit result");
        function
            .append_block_parameter(normal, resource)
            .expect("normal writeback");
        function
            .append_block_parameter(fault, resource)
            .expect("fault writeback");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::ResourceClose {
                        kind: ResourceKind::File,
                        resource: value,
                        normal: ResultTarget::new(normal, []),
                        fault: UnwindTarget::new(fault, []),
                    },
                    origin(0),
                ),
            )
            .expect("resource close");
        function
            .terminate(
                normal,
                Terminator::new(TerminatorKind::Return(returned), origin(0)),
            )
            .expect("return");
        function
            .terminate(
                fault,
                Terminator::new(TerminatorKind::ResumeFault, origin(0)),
            )
            .expect("resume fault");
    }
    program.finish()
}

fn assert_has_code(program: Program, expected: ValidationCode) {
    let errors = program
        .into_checked()
        .expect_err("malformed cleanup LCIR must fail");
    assert!(
        errors
            .as_slice()
            .iter()
            .any(|error| error.code() == expected),
        "missing {expected:?}: {:#?}",
        errors.as_slice()
    );
}

#[test]
fn typed_resource_cleanup_has_exact_runtime_and_fault_edges() {
    let checked = resource_program(
        &[Type::Int],
        Effects::MAY_FAULT.union(Effects::NEEDS_RUNTIME),
    )
    .into_checked()
    .expect("one-Int typed resource cleanup is valid");
    let dump = dump_program(&checked);
    assert!(
        dump.contains(
            "resource.close.file %v0, normal b1(result, writeback0), fault b2(writeback0)"
        ),
        "{dump}"
    );
    assert!(dump.contains("effects=may_fault+needs_runtime"), "{dump}");
}

#[test]
fn typed_resource_cleanup_rejects_noncanonical_shape_and_missing_runtime_effect() {
    assert_has_code(
        resource_program(
            &[Type::Int, Type::Int],
            Effects::MAY_FAULT.union(Effects::NEEDS_RUNTIME),
        ),
        ValidationCode::TypeMismatch,
    );
    assert_has_code(
        resource_program(&[Type::Int], Effects::MAY_FAULT),
        ValidationCode::EffectMismatch,
    );
}

#[test]
fn active_resource_cleanup_preserves_the_primary_on_both_close_outcomes() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let boolean = program.type_id(&Type::Bool).expect("Bool");
    let unit = program.type_id(&Type::Unit).expect("Unit");
    let resource = program
        .add_pod_record_type(Type::Nominal(TypeId(91), Vec::new()), &[Type::Int])
        .expect("resource product");
    let function = program
        .declare_function(
            origin(1),
            "cleanup.resource.primary",
            Signature::new([boolean, resource], unit),
            Effects::MAY_FAULT.union(Effects::NEEDS_RUNTIME),
        )
        .expect("declare active cleanup");
    {
        let mut function = program.function(function).expect("function builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let cleanup = function.create_block().expect("cleanup");
        let close_normal = function.create_block().expect("close normal");
        let close_fault = function.create_block().expect("close fault");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_block_parameter(entry, boolean)
            .expect("condition");
        let value = function
            .append_block_parameter(entry, resource)
            .expect("resource");
        function
            .append_block_parameter(close_normal, unit)
            .expect("close result");
        function
            .append_block_parameter(close_normal, resource)
            .expect("normal writeback");
        function
            .append_block_parameter(close_fault, resource)
            .expect("fault writeback");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Assert {
                        condition,
                        metadata: ContractFaultMetadata::assertion(origin(1).span),
                        success: BlockTarget::new(normal, []),
                        fault: UnwindTarget::new(cleanup, []),
                    },
                    origin(1),
                ),
            )
            .expect("assert");
        let returned = function
            .append_instruction(
                normal,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(1),
            )
            .expect("Unit")[0];
        function
            .terminate(
                normal,
                Terminator::new(TerminatorKind::Return(returned), origin(1)),
            )
            .expect("return");
        function
            .terminate(
                cleanup,
                Terminator::new(
                    TerminatorKind::ResourceClose {
                        kind: ResourceKind::Socket,
                        resource: value,
                        normal: ResultTarget::new(close_normal, []),
                        fault: UnwindTarget::new(close_fault, []),
                    },
                    origin(1),
                ),
            )
            .expect("active resource close");
        for block in [close_normal, close_fault] {
            function
                .terminate(
                    block,
                    Terminator::new(TerminatorKind::ResumeFault, origin(1)),
                )
                .expect("resume primary");
        }
    }
    program
        .finish_checked()
        .expect("active close success and secondary fault both preserve the primary");

    // LCIR has no suspension operation. Advertising suspension on this same
    // cleanup graph cannot smuggle it through validation as an unused effect.
    let suspending = Effects::MAY_FAULT
        .union(Effects::NEEDS_RUNTIME)
        .union(Effects::NEEDS_EXECUTOR)
        .union(Effects::MAY_SUSPEND);
    assert_has_code(
        resource_program(&[Type::Int], suspending),
        ValidationCode::EffectMismatch,
    );
}
