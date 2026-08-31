use loom_codegen_ir::{
    BlockTarget, BuildErrorCode, CanonicalTypeCatalog, Effects, InstructionKind, Origin, Program,
    ProgramBuilder, Signature, SumCase, TargetLayout, Terminator, TerminatorKind, ValidationCode,
    validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

fn origin(source: u32) -> Origin {
    Origin::synthetic(FunctionId(source))
}

#[derive(Clone, Copy)]
enum ProductCase {
    Move,
    ExtractTask,
    Insert,
}

#[expect(
    clippy::too_many_lines,
    reason = "one compact fixture keeps valid and rejected product ownership flows structurally comparable"
)]
fn product_carrier_program(case: ProductCase) -> Program {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let integer = builder.type_id(&Type::Int).expect("Int");
    let task_semantic = Type::Task(Box::new(Type::Int));
    let task = builder
        .add_task_handle_type(task_semantic.clone())
        .expect("Task[Int]");
    let carrier_semantic = Type::Nominal(TypeId(81), Vec::new());
    let carrier = builder
        .add_pod_record_type(carrier_semantic, &[task_semantic.clone(), Type::Int])
        .expect("Task-bearing product");

    if matches!(case, ProductCase::Move) {
        let identity = builder
            .declare_function(
                origin(81),
                "task_carrier.identity",
                Signature::new([carrier], carrier),
                Effects::NONE,
            )
            .expect("identity");
        {
            let mut function = builder.function(identity).expect("identity builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            let value = function
                .append_block_parameter(entry, carrier)
                .expect("carrier");
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(value), origin(81)),
                )
                .expect("return");
        }

        let caller = builder
            .declare_function(
                origin(82),
                "task_carrier.move",
                Signature::new([task, integer], carrier),
                Effects::NONE,
            )
            .expect("caller");
        {
            let mut function = builder.function(caller).expect("caller builder");
            let entry = function.create_block().expect("entry");
            let forwarded = function.create_block().expect("forwarded");
            function.set_entry(entry).expect("set entry");
            let child = function
                .append_block_parameter(entry, task)
                .expect("Task input");
            let label = function
                .append_block_parameter(entry, integer)
                .expect("label input");
            let aggregate = function
                .append_instruction(
                    entry,
                    InstructionKind::ProductConstruct {
                        fields: Box::from([child, label]),
                    },
                    &[carrier],
                    origin(82),
                )
                .expect("construct")[0];
            function
                .append_instruction(
                    entry,
                    InstructionKind::ProductExtract {
                        aggregate,
                        field: 1,
                    },
                    &[integer],
                    origin(82),
                )
                .expect("borrow task-free field");
            let forwarded_value = function
                .append_block_parameter(forwarded, carrier)
                .expect("forwarded carrier");
            function
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Jump(BlockTarget::new(forwarded, [aggregate])),
                        origin(82),
                    ),
                )
                .expect("forward");
            let returned = function
                .append_instruction(
                    forwarded,
                    InstructionKind::DirectCall {
                        callee: identity,
                        arguments: Box::from([forwarded_value]),
                    },
                    &[carrier],
                    origin(82),
                )
                .expect("call")[0];
            function
                .terminate(
                    forwarded,
                    Terminator::new(TerminatorKind::Return(returned), origin(82)),
                )
                .expect("return");
        }
    } else {
        let result = if matches!(case, ProductCase::ExtractTask) {
            task
        } else {
            carrier
        };
        let parameters = if matches!(case, ProductCase::Insert) {
            vec![carrier, integer]
        } else {
            vec![carrier]
        };
        let function_id = builder
            .declare_function(
                origin(83),
                "task_carrier.invalid_product_operation",
                Signature::new(parameters.clone(), result),
                Effects::NONE,
            )
            .expect("invalid product fixture");
        let mut function = builder.function(function_id).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let parameters = parameters
            .into_iter()
            .map(|ty| {
                function
                    .append_block_parameter(entry, ty)
                    .expect("parameter")
            })
            .collect::<Vec<_>>();
        let value = match case {
            ProductCase::ExtractTask => function
                .append_instruction(
                    entry,
                    InstructionKind::ProductExtract {
                        aggregate: parameters[0],
                        field: 0,
                    },
                    &[task],
                    origin(83),
                )
                .expect("extract")[0],
            ProductCase::Insert => function
                .append_instruction(
                    entry,
                    InstructionKind::ProductInsert {
                        aggregate: parameters[0],
                        field: 1,
                        value: parameters[1],
                    },
                    &[carrier],
                    origin(83),
                )
                .expect("insert")[0],
            ProductCase::Move => unreachable!(),
        };
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), origin(83)),
            )
            .expect("return");
    }
    builder.finish()
}

#[test]
fn product_task_carriers_move_while_task_free_extraction_borrows() {
    validate_program(&product_carrier_program(ProductCase::Move))
        .unwrap_or_else(|errors| panic!("valid affine product flow failed: {errors:#?}"));
}

#[test]
fn transparent_task_carriers_forward_and_unrefine_by_move() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let task_semantic = Type::Task(Box::new(Type::Int));
    let task = builder
        .add_task_handle_type(task_semantic.clone())
        .expect("Task[Int]");
    let pending_semantic = Type::Nominal(TypeId(89), Vec::new());
    let pending = builder
        .add_transparent_type(pending_semantic, &task_semantic)
        .expect("transparent Task wrapper");
    let forward = builder
        .declare_function(
            origin(89),
            "task_carrier.transparent_forward",
            Signature::new([pending], pending),
            Effects::NONE,
        )
        .expect("forward declaration");
    {
        let mut function = builder.function(forward).expect("forward builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, pending)
            .expect("transparent Task input");
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(value), origin(89)),
            )
            .expect("return transparent Task");
    }
    let round_trip = builder
        .declare_function(
            origin(90),
            "task_carrier.transparent_round_trip",
            Signature::new([pending], task),
            Effects::NONE,
        )
        .expect("round-trip declaration");
    {
        let mut function = builder.function(round_trip).expect("round-trip builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, pending)
            .expect("transparent Task input");
        let forwarded = function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: forward,
                    arguments: Box::from([value]),
                },
                &[pending],
                origin(90),
            )
            .expect("forward transparent Task")[0];
        let unrefined = function
            .append_instruction(
                entry,
                InstructionKind::Unrefine { value: forwarded },
                &[task],
                origin(90),
            )
            .expect("unrefine Task")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(unrefined), origin(90)),
            )
            .expect("return Task");
    }

    validate_program(&builder.finish())
        .unwrap_or_else(|errors| panic!("valid transparent Task flow failed: {errors:#?}"));
}

#[test]
fn task_bearing_product_extract_and_affine_insert_are_rejected() {
    for case in [ProductCase::ExtractTask, ProductCase::Insert] {
        let errors = validate_program(&product_carrier_program(case))
            .expect_err("unsupported affine product operation was accepted");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InvalidTaskOwnership
                && (error
                    .message()
                    .contains("cannot split a Task-bearing field")
                    || error.message().contains("cannot rebuild or mutate"))
        }));
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one fixture keeps valid case-payload ownership and duplicate scrutinee forwarding directly comparable"
)]
fn sum_carrier_program(duplicate_scrutinee: bool) -> Program {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let task_semantic = Type::Task(Box::new(Type::Int));
    let task = builder
        .add_task_handle_type(task_semantic.clone())
        .expect("Task[Int]");
    let payload_semantic = Type::Nominal(TypeId(84), Vec::new());
    let payload = builder
        .add_pod_record_type(payload_semantic.clone(), &[task_semantic])
        .expect("nested Task-bearing product");
    let sum = builder
        .add_sum_type(
            Type::Nominal(TypeId(85), Vec::new()),
            &[
                Box::from([payload_semantic.clone()]),
                Box::from([payload_semantic]),
            ],
        )
        .expect("Task-bearing sum");
    let parameters = if duplicate_scrutinee {
        vec![sum]
    } else {
        vec![task]
    };
    let function_id = builder
        .declare_function(
            origin(84),
            "task_carrier.sum_switch",
            Signature::new(parameters.clone(), payload),
            Effects::NONE,
        )
        .expect("sum fixture");
    {
        let mut function = builder.function(function_id).expect("function builder");
        let entry = function.create_block().expect("entry");
        let first = function.create_block().expect("first case");
        let second = function.create_block().expect("second case");
        function.set_entry(entry).expect("set entry");
        let input = function
            .append_block_parameter(entry, parameters[0])
            .expect("input");
        let scrutinee = if duplicate_scrutinee {
            input
        } else {
            let payload_value = function
                .append_instruction(
                    entry,
                    InstructionKind::ProductConstruct {
                        fields: Box::from([input]),
                    },
                    &[payload],
                    origin(84),
                )
                .expect("construct nested product")[0];
            function
                .append_instruction(
                    entry,
                    InstructionKind::SumConstruct {
                        variant: 0,
                        payload: Box::from([payload_value]),
                    },
                    &[sum],
                    origin(84),
                )
                .expect("construct")[0]
        };
        let mut payloads = Vec::new();
        for case in [first, second] {
            payloads.push(
                function
                    .append_block_parameter(case, payload)
                    .expect("implicit affine product payload"),
            );
            if duplicate_scrutinee {
                function
                    .append_block_parameter(case, sum)
                    .expect("explicit duplicate carrier");
            }
        }
        let arguments = if duplicate_scrutinee {
            vec![scrutinee]
        } else {
            Vec::new()
        };
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::SumSwitch {
                        scrutinee,
                        cases: Box::from([
                            SumCase::new(0, first, arguments.clone()),
                            SumCase::new(1, second, arguments),
                        ]),
                    },
                    origin(84),
                ),
            )
            .expect("switch");
        for (case, payload) in [first, second].into_iter().zip(payloads) {
            function
                .terminate(
                    case,
                    Terminator::new(TerminatorKind::Return(payload), origin(84)),
                )
                .expect("return payload");
        }
    }
    builder.finish()
}

#[test]
fn sum_switch_consumes_the_scrutinee_and_owns_the_selected_affine_payload() {
    validate_program(&sum_carrier_program(false))
        .unwrap_or_else(|errors| panic!("valid affine sum flow failed: {errors:#?}"));

    let errors = validate_program(&sum_carrier_program(true))
        .expect_err("sum.switch duplicated its affine scrutinee onto an explicit edge");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidTaskOwnership
            && error.message().contains("moved more than once")
    }));
}

#[expect(
    clippy::too_many_lines,
    reason = "one fixture keeps the complete structural borrow chain and its rejected consuming use directly comparable"
)]
fn borrowed_sum_program(consume_borrowed: bool) -> Program {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let integer = builder.type_id(&Type::Int).expect("Int");
    let task_semantic = Type::Task(Box::new(Type::Int));
    let task = builder
        .add_task_handle_type(task_semantic.clone())
        .expect("Task[Int]");
    let pending_semantic = Type::Nominal(TypeId(91), Vec::new());
    let pending = builder
        .add_transparent_type(pending_semantic.clone(), &task_semantic)
        .expect("transparent Task wrapper");
    let carrier_semantic = Type::Nominal(TypeId(92), Vec::new());
    let carrier = builder
        .add_pod_record_type(carrier_semantic.clone(), &[pending_semantic, Type::Int])
        .expect("Task-bearing product");
    let sum = builder
        .add_sum_type(
            Type::Nominal(TypeId(93), Vec::new()),
            &[Box::from([carrier_semantic])],
        )
        .expect("Task-bearing sum");
    let result = if consume_borrowed { task } else { sum };
    let function_id = builder
        .declare_function(
            origin(91),
            "task_carrier.borrowed_inspection",
            Signature::new([sum], result),
            Effects::NONE,
        )
        .expect("borrowed inspection declaration");
    {
        let mut function = builder.function(function_id).expect("function builder");
        let entry = function.create_block().expect("entry");
        let selected = function.create_block().expect("selected case");
        function.set_entry(entry).expect("set entry");
        let owner = function
            .append_block_parameter(entry, sum)
            .expect("owned sum");
        let borrowed_owner = function
            .append_instruction(
                entry,
                InstructionKind::TaskCarrierBorrow { value: owner },
                &[sum],
                origin(91),
            )
            .expect("borrow whole carrier")[0];
        let borrowed_carrier = function
            .append_block_parameter(selected, carrier)
            .expect("borrowed carrier");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::SumBorrowSwitch {
                        scrutinee: borrowed_owner,
                        cases: Box::from([SumCase::new(0, selected, [])]),
                    },
                    origin(91),
                ),
            )
            .expect("borrowed switch");
        let borrowed_pending = function
            .append_instruction(
                selected,
                InstructionKind::ProductBorrow {
                    aggregate: borrowed_carrier,
                    field: 0,
                },
                &[pending],
                origin(91),
            )
            .expect("borrow product field")[0];
        let borrowed_task = function
            .append_instruction(
                selected,
                InstructionKind::UnrefineBorrow {
                    value: borrowed_pending,
                },
                &[task],
                origin(91),
            )
            .expect("borrow transparent base")[0];
        function
            .append_instruction(
                selected,
                InstructionKind::ProductExtract {
                    aggregate: borrowed_carrier,
                    field: 1,
                },
                &[integer],
                origin(91),
            )
            .expect("read task-free sibling");
        function
            .terminate(
                selected,
                Terminator::new(
                    TerminatorKind::Return(if consume_borrowed {
                        borrowed_task
                    } else {
                        owner
                    }),
                    origin(91),
                ),
            )
            .expect("return");
    }
    builder.finish()
}

#[test]
fn borrowed_affine_inspection_preserves_the_owner_and_cannot_regain_ownership() {
    validate_program(&borrowed_sum_program(false))
        .unwrap_or_else(|errors| panic!("valid borrowed inspection failed: {errors:#?}"));

    let errors = validate_program(&borrowed_sum_program(true))
        .expect_err("a borrowed Task alias regained ownership at return");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidTaskOwnership
            && error
                .message()
                .contains("consumed more than once or is unavailable")
    }));
}

#[test]
fn sum_zip_switch_rejects_task_bearing_operands_before_mismatch_can_drop_them() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let task_semantic = Type::Task(Box::new(Type::Int));
    let task = builder
        .add_task_handle_type(task_semantic.clone())
        .expect("Task[Int]");
    let sum = builder
        .add_sum_type(
            Type::Nominal(TypeId(86), Vec::new()),
            &[Box::from([task_semantic]), Box::new([])],
        )
        .expect("task-bearing sum");
    let function_id = builder
        .declare_function(
            origin(86),
            "task_sum.zip_rejected",
            Signature::new([sum, sum], unit),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = builder.function(function_id).expect("function builder");
        let entry = function.create_block().expect("entry");
        let payload = function.create_block().expect("payload case");
        let empty = function.create_block().expect("empty case");
        let mismatch = function.create_block().expect("mismatch");
        function.set_entry(entry).expect("set entry");
        let left = function
            .append_block_parameter(entry, sum)
            .expect("left sum");
        let right = function
            .append_block_parameter(entry, sum)
            .expect("right sum");
        function
            .append_block_parameter(payload, task)
            .expect("left task payload");
        function
            .append_block_parameter(payload, task)
            .expect("right task payload");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::SumZipSwitch {
                        left,
                        right,
                        cases: Box::from([
                            SumCase::new(0, payload, []),
                            SumCase::new(1, empty, []),
                        ]),
                        mismatch: BlockTarget::new(mismatch, []),
                    },
                    origin(86),
                ),
            )
            .expect("unchecked zip switch");
        for block in [payload, empty, mismatch] {
            let result = function
                .append_instruction(
                    block,
                    InstructionKind::Constant(loom_codegen_ir::Constant::Unit),
                    &[unit],
                    origin(86),
                )
                .expect("Unit")[0];
            function
                .terminate(
                    block,
                    Terminator::new(TerminatorKind::Return(result), origin(86)),
                )
                .expect("return");
        }
    }

    let errors = validate_program(&builder.finish()).expect_err("task-bearing zip must fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidTaskOwnership
            && error
                .message()
                .contains("cannot discard a task-bearing payload")
    }));
}

#[test]
fn nested_task_lists_and_repeated_affine_aggregates_are_rejected() {
    let text_map_id = TypeId(85);
    let mut builder = ProgramBuilder::with_canonical_types(
        TargetLayout::new(64).expect("target"),
        CanonicalTypeCatalog {
            text_map: Some(text_map_id),
            ..CanonicalTypeCatalog::default()
        },
    );
    builder.add_managed_text_type().expect("managed Text");
    let task_semantic = Type::Task(Box::new(Type::Int));
    builder
        .add_task_handle_type(task_semantic.clone())
        .expect("Task[Int]");
    let task_list_semantic = Type::List(Box::new(task_semantic.clone()));
    builder
        .add_managed_list_type(task_list_semantic.clone())
        .expect("top-level List[Task[Int]] carrier");
    assert_eq!(
        builder
            .add_transparent_type(Type::Nominal(TypeId(91), Vec::new()), &task_list_semantic)
            .expect_err("a transparent carrier must not hide List[Task[Int]]")
            .code(),
        BuildErrorCode::InvalidValueType
    );
    let wrapped_task_semantic = Type::Nominal(TypeId(90), Vec::new());
    builder
        .add_transparent_type(wrapped_task_semantic.clone(), &task_semantic)
        .expect("transparent Task carrier");
    builder
        .add_managed_list_type(Type::List(Box::new(wrapped_task_semantic)))
        .expect("raw non-exact TaskHandle List");
    builder
        .add_pod_record_type(
            Type::Nominal(TypeId(87), Vec::new()),
            std::slice::from_ref(&task_list_semantic),
        )
        .expect("raw product containing List[Task]");
    builder
        .add_sum_type(
            Type::Nominal(TypeId(88), Vec::new()),
            &[Box::from([task_list_semantic])],
        )
        .expect("raw sum containing List[Task]");
    let carrier_semantic = Type::Nominal(TypeId(86), Vec::new());
    builder
        .add_pod_record_type(carrier_semantic.clone(), &[task_semantic])
        .expect("affine product");
    builder
        .add_managed_list_type(Type::List(Box::new(carrier_semantic.clone())))
        .expect("raw repeated affine product");
    builder
        .add_managed_text_map_type(Type::Nominal(text_map_id, vec![carrier_semantic]))
        .expect("raw affine TextMap value");

    let errors = validate_program(&builder.finish())
        .expect_err("repeated and keyed affine aggregate storage was accepted");
    assert_eq!(
        errors
            .as_slice()
            .iter()
            .filter(|error| {
                error.code() == ValidationCode::RepresentationPlan
                    && error
                        .message()
                        .contains("top-level affine carrier and cannot be nested")
            })
            .count(),
        2,
        "products and sums must independently reject nested List[Task]: {errors:#?}"
    );
    assert_eq!(
        errors
            .as_slice()
            .iter()
            .filter(|error| {
                error.code() == ValidationCode::RepresentationPlan
                    && error.path().ends_with("managed_pointer")
            })
            .count(),
        3,
        "repeated affine products, keyed affine products, and non-exact TaskHandle Lists must fail independently: {errors:#?}"
    );
}
