use loom_codegen_ir::{
    BlockTarget, Constant, Effects, FaultCode, InstructionKind, Origin, ProgramBuilder,
    ResultTarget, Signature, TargetLayout, Terminator, TerminatorKind, UnwindTarget,
    ValidationCode, dump_program, validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

fn origin(function: u32) -> Origin {
    Origin::synthetic(FunctionId(function))
}

#[test]
#[allow(clippy::too_many_lines)]
fn products_cross_construction_projection_phi_call_return_and_fault_edges() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let counter_semantic = Type::Nominal(TypeId(40), Vec::new());
    let counter = builder
        .add_pod_record_type(counter_semantic.clone(), &[Type::Int, Type::Int])
        .expect("Counter product");
    let holder = builder
        .add_pod_record_type(
            Type::Nominal(TypeId(41), Vec::new()),
            &[counter_semantic, Type::Bool],
        )
        .expect("nested Holder product");

    let update = builder
        .declare_function(
            origin(0),
            "aggregate.update_then_fault",
            Signature::with_inout_params([counter, integer], unit, [0_u32]),
            Effects::MAY_FAULT,
        )
        .expect("declare update");
    let choose = builder
        .declare_function(
            origin(1),
            "aggregate.choose",
            Signature::new([boolean, holder, holder], holder),
            Effects::NONE,
        )
        .expect("declare choose");
    let caller = builder
        .declare_function(
            origin(2),
            "aggregate.invoke",
            Signature::new(Vec::new(), unit),
            Effects::MAY_FAULT,
        )
        .expect("declare caller");

    {
        let mut function = builder.function(update).expect("update builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let receiver = function
            .append_block_parameter(entry, counter)
            .expect("receiver");
        let replacement = function
            .append_block_parameter(entry, integer)
            .expect("replacement");
        let updated = function
            .append_instruction(
                entry,
                InstructionKind::ProductInsert {
                    aggregate: receiver,
                    field: 0,
                    value: replacement,
                },
                &[counter],
                origin(0),
            )
            .expect("insert")[0];
        function
            .terminate(
                entry,
                Terminator::with_writebacks(
                    TerminatorKind::Fault {
                        code: FaultCode::ContractFailed,
                    },
                    origin(0),
                    [updated],
                ),
            )
            .expect("fault");
    }

    {
        let mut function = builder.function(choose).expect("choose builder");
        let entry = function.create_block().expect("entry");
        let left = function.create_block().expect("left");
        let right = function.create_block().expect("right");
        let join = function.create_block().expect("join");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_block_parameter(entry, boolean)
            .expect("condition");
        let first = function
            .append_block_parameter(entry, holder)
            .expect("first");
        let second = function
            .append_block_parameter(entry, holder)
            .expect("second");
        let merged = function
            .append_block_parameter(join, holder)
            .expect("merged");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(left, []),
                        else_target: BlockTarget::new(right, []),
                    },
                    origin(1),
                ),
            )
            .expect("branch");
        function
            .terminate(
                left,
                Terminator::new(
                    TerminatorKind::Jump(BlockTarget::new(join, [first])),
                    origin(1),
                ),
            )
            .expect("left jump");
        function
            .terminate(
                right,
                Terminator::new(
                    TerminatorKind::Jump(BlockTarget::new(join, [second])),
                    origin(1),
                ),
            )
            .expect("right jump");
        function
            .terminate(
                join,
                Terminator::new(TerminatorKind::Return(merged), origin(1)),
            )
            .expect("return");
    }

    {
        let mut function = builder.function(caller).expect("caller builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let unwind = function.create_block().expect("unwind");
        function.set_entry(entry).expect("set entry");
        let zero = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(0)),
                &[integer],
                origin(2),
            )
            .expect("zero")[0];
        let one = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(1)),
                &[integer],
                origin(2),
            )
            .expect("one")[0];
        let product = function
            .append_instruction(
                entry,
                InstructionKind::ProductConstruct {
                    fields: Box::from([zero, one]),
                },
                &[counter],
                origin(2),
            )
            .expect("construct")[0];
        let returned = function
            .append_block_parameter(normal, unit)
            .expect("source result");
        let normal_writeback = function
            .append_block_parameter(normal, counter)
            .expect("normal writeback");
        let unwind_writeback = function
            .append_block_parameter(unwind, counter)
            .expect("unwind writeback");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Invoke {
                        callee: update,
                        arguments: Box::from([product, one]),
                        normal: ResultTarget::new(normal, []),
                        unwind: UnwindTarget::new(unwind, []),
                    },
                    origin(2),
                ),
            )
            .expect("invoke");
        let extracted = function
            .append_instruction(
                normal,
                InstructionKind::ProductExtract {
                    aggregate: normal_writeback,
                    field: 1,
                },
                &[integer],
                origin(2),
            )
            .expect("extract")[0];
        let rebuilt = function
            .append_instruction(
                normal,
                InstructionKind::ProductInsert {
                    aggregate: normal_writeback,
                    field: 1,
                    value: extracted,
                },
                &[counter],
                origin(2),
            )
            .expect("reinsert")[0];
        let _ = (returned, rebuilt, unwind_writeback);
        function
            .terminate(
                normal,
                Terminator::new(TerminatorKind::Return(returned), origin(2)),
            )
            .expect("normal return");
        function
            .terminate(
                unwind,
                Terminator::new(TerminatorKind::ResumeFault, origin(2)),
            )
            .expect("resume");
    }

    let checked = builder.finish_checked().expect("valid aggregate LCIR");
    let dump = dump_program(&checked);
    assert!(dump.contains("product.construct"), "{dump}");
    assert!(dump.contains("product.extract"), "{dump}");
    assert!(dump.contains("product.insert"), "{dump}");
    assert!(dump.contains("writebacks("), "{dump}");
}

#[test]
fn malformed_product_and_inout_shapes_are_rejected_together() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let product = builder
        .add_pod_record_type(
            Type::Nominal(TypeId(50), Vec::new()),
            &[Type::Int, Type::Int],
        )
        .expect("product");
    let function = builder
        .declare_function(
            origin(3),
            "aggregate.malformed",
            Signature::with_inout_params([product], unit, [0_u32, 0_u32]),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut body = builder.function(function).expect("body");
        let entry = body.create_block().expect("entry");
        body.set_entry(entry).expect("set entry");
        let receiver = body
            .append_block_parameter(entry, product)
            .expect("receiver");
        let field = body
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(1)),
                &[integer],
                origin(3),
            )
            .expect("field")[0];
        let _malformed = body
            .append_instruction(
                entry,
                InstructionKind::ProductConstruct {
                    fields: Box::from([field]),
                },
                &[product],
                origin(3),
            )
            .expect("malformed construct")[0];
        let unit_value = body
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(3),
            )
            .expect("unit")[0];
        body.terminate(
            entry,
            Terminator::with_writebacks(TerminatorKind::Return(unit_value), origin(3), [receiver]),
        )
        .expect("return");
    }

    let errors = validate_program(&builder.finish()).expect_err("malformed LCIR must fail");
    let codes = errors
        .as_slice()
        .iter()
        .map(loom_codegen_ir::ValidationError::code)
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&ValidationCode::InstructionShape),
        "{errors:?}"
    );
    assert!(codes.contains(&ValidationCode::InOutShape), "{errors:?}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn malformed_product_calls_and_implicit_writeback_edges_are_rejected() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let product = builder
        .add_pod_record_type(
            Type::Nominal(TypeId(51), Vec::new()),
            &[Type::Int, Type::Int],
        )
        .expect("product");
    let direct_callee = builder
        .declare_function(
            origin(4),
            "aggregate.direct_callee",
            Signature::with_inout_params([product], unit, [0_u32]),
            Effects::NONE,
        )
        .expect("declare direct callee");
    let fallible_callee = builder
        .declare_function(
            origin(5),
            "aggregate.fallible_callee",
            Signature::with_inout_params([product], unit, [0_u32]),
            Effects::MAY_FAULT,
        )
        .expect("declare fallible callee");
    let malformed_direct = builder
        .declare_function(
            origin(6),
            "aggregate.malformed_direct",
            Signature::new([product], unit),
            Effects::NONE,
        )
        .expect("declare direct caller");
    let malformed_invoke = builder
        .declare_function(
            origin(7),
            "aggregate.malformed_invoke",
            Signature::new([product], unit),
            Effects::MAY_FAULT,
        )
        .expect("declare invoke caller");
    let malformed_projection = builder
        .declare_function(
            origin(8),
            "aggregate.malformed_projection",
            Signature::new([product], unit),
            Effects::NONE,
        )
        .expect("declare projection caller");

    {
        let mut function = builder.function(direct_callee).expect("direct callee");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let receiver = function
            .append_block_parameter(entry, product)
            .expect("receiver");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(4),
            )
            .expect("unit")[0];
        function
            .terminate(
                entry,
                Terminator::with_writebacks(TerminatorKind::Return(result), origin(4), [receiver]),
            )
            .expect("return");
    }

    {
        let mut function = builder.function(fallible_callee).expect("fallible callee");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let receiver = function
            .append_block_parameter(entry, product)
            .expect("receiver");
        function
            .terminate(
                entry,
                Terminator::with_writebacks(
                    TerminatorKind::Fault {
                        code: FaultCode::ContractFailed,
                    },
                    origin(5),
                    [receiver],
                ),
            )
            .expect("fault");
    }

    {
        let mut function = builder.function(malformed_direct).expect("direct caller");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let receiver = function
            .append_block_parameter(entry, product)
            .expect("receiver");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: direct_callee,
                    arguments: Box::from([receiver]),
                },
                &[unit],
                origin(6),
            )
            .expect("malformed call result")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin(6)),
            )
            .expect("return");
    }

    {
        let mut function = builder.function(malformed_invoke).expect("invoke caller");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let unwind = function.create_block().expect("unwind");
        function.set_entry(entry).expect("set entry");
        let receiver = function
            .append_block_parameter(entry, product)
            .expect("receiver");
        let result = function
            .append_block_parameter(normal, unit)
            .expect("source result only");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Invoke {
                        callee: fallible_callee,
                        arguments: Box::from([receiver]),
                        normal: ResultTarget::new(normal, []),
                        unwind: UnwindTarget::new(unwind, []),
                    },
                    origin(7),
                ),
            )
            .expect("invoke");
        function
            .terminate(
                normal,
                Terminator::new(TerminatorKind::Return(result), origin(7)),
            )
            .expect("normal return");
        function
            .terminate(
                unwind,
                Terminator::new(TerminatorKind::ResumeFault, origin(7)),
            )
            .expect("resume fault");
    }

    {
        let mut function = builder
            .function(malformed_projection)
            .expect("projection caller");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let aggregate = function
            .append_block_parameter(entry, product)
            .expect("aggregate");
        let wrong_field = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[boolean],
                origin(8),
            )
            .expect("wrong field")[0];
        let _out_of_range = function
            .append_instruction(
                entry,
                InstructionKind::ProductExtract {
                    aggregate,
                    field: 9,
                },
                &[integer],
                origin(8),
            )
            .expect("out-of-range extract")[0];
        let _wrong_insert = function
            .append_instruction(
                entry,
                InstructionKind::ProductInsert {
                    aggregate,
                    field: 0,
                    value: wrong_field,
                },
                &[product],
                origin(8),
            )
            .expect("wrong insert")[0];
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(8),
            )
            .expect("unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin(8)),
            )
            .expect("return");
    }

    let errors = validate_program(&builder.finish()).expect_err("malformed LCIR must fail");
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstructionShape
                && error.path().starts_with("function[2].instruction")
        }),
        "{errors:?}"
    );
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::BlockArgument && error.path().contains("normal")
        }),
        "{errors:?}"
    );
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::BlockArgument && error.path().contains("unwind")
        }),
        "{errors:?}"
    );
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstructionShape
                && error.message().contains("out of range")
        }),
        "{errors:?}"
    );
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::TypeMismatch && error.path().contains(".value")
        }),
        "{errors:?}"
    );
}
