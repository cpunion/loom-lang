use loom_codegen_ir::{
    BuildErrorCode, Constant, Effects, InstructionKind, Origin, ProgramBuilder, Signature,
    TargetLayout, Terminator, TerminatorKind, ValidationCode, ValueTypeKind, validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

fn target() -> TargetLayout {
    TargetLayout::new(64).expect("test target")
}

#[test]
fn transparent_nominal_values_keep_semantic_identity_and_share_only_physical_repr() {
    let money = Type::Nominal(TypeId(100), Vec::new());
    let mut builder = ProgramBuilder::new(target());
    let float = builder.type_id(&Type::Float).expect("Float type");
    let money_id = builder
        .add_transparent_type(money.clone(), &Type::Float)
        .expect("transparent Money type");
    assert_ne!(money_id, float);
    let representations = builder.representations();
    let float_repr = representations
        .value_type(float)
        .expect("Float value")
        .repr();
    let money_value = representations.value_type(money_id).expect("Money value");
    assert_eq!(money_value.repr(), float_repr);
    assert_eq!(
        money_value.kind(),
        ValueTypeKind::Transparent { base: float }
    );

    validate_program(&builder.finish()).expect("transparent representation plan");
}

#[test]
fn functional_inout_does_not_admit_unit_or_transparent_scalars() {
    let money = Type::Nominal(TypeId(104), Vec::new());
    let mut builder = ProgramBuilder::new(target());
    let unit = builder.type_id(&Type::Unit).expect("Unit type");
    let money = builder
        .add_transparent_type(money, &Type::Float)
        .expect("transparent Money type");
    builder
        .declare_function(
            Origin::synthetic(FunctionId(104)),
            "invalid_scalar_inout",
            Signature::with_inout_params([unit, money], unit, [0_u32, 1_u32]),
            Effects::NONE,
        )
        .expect("declare invalid raw signature");

    let errors = validate_program(&builder.finish()).expect_err("invalid inout types must fail");
    let inout = errors
        .as_slice()
        .iter()
        .filter(|error| error.code() == ValidationCode::InOutShape)
        .collect::<Vec<_>>();
    assert_eq!(inout.len(), 2, "{errors:?}");
    assert!(inout.iter().any(|error| error.path().ends_with("inout[0]")));
    assert!(inout.iter().any(|error| error.path().ends_with("inout[1]")));
}

#[test]
fn functional_inout_admits_canonical_task_free_sums() {
    let mut builder = ProgramBuilder::new(target());
    let unit = builder.type_id(&Type::Unit).expect("Unit type");
    let choice = builder
        .add_sum_type(
            Type::Nominal(TypeId(105), Vec::new()),
            &[Box::new([]), Box::from([Type::Int])],
        )
        .expect("closed task-free sum");
    let origin = Origin::synthetic(FunctionId(105));
    let function = builder
        .declare_function(
            origin,
            "replace_choice",
            Signature::with_inout_params([choice], unit, [0_u32]),
            Effects::NONE,
        )
        .expect("declare sum inout");
    {
        let mut body = builder.function(function).expect("function body");
        let entry = body.create_block().expect("entry");
        body.set_entry(entry).expect("set entry");
        let receiver = body
            .append_block_parameter(entry, choice)
            .expect("receiver");
        let result = body
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit result")[0];
        body.terminate(
            entry,
            Terminator::with_writebacks(TerminatorKind::Return(result), origin, [receiver]),
        )
        .expect("return receiver writeback");
    }

    builder
        .finish_checked()
        .expect("canonical task-free sum inout must validate");
}

#[test]
fn functional_inout_rejects_task_hidden_behind_a_managed_sum_payload() {
    let mut builder = ProgramBuilder::new(target());
    let unit = builder.type_id(&Type::Unit).expect("Unit type");
    let task_semantic = Type::Task(Box::new(Type::Int));
    builder
        .add_task_handle_type(task_semantic.clone())
        .expect("Task[Int]");
    let task_list_semantic = Type::List(Box::new(task_semantic));
    builder
        .add_managed_list_type(task_list_semantic.clone())
        .expect("List[Task[Int]]");
    let carrier = builder
        .add_sum_type(
            Type::Nominal(TypeId(106), Vec::new()),
            &[Box::new([]), Box::from([task_list_semantic])],
        )
        .expect("sum containing a managed Task list");
    builder
        .declare_function(
            Origin::synthetic(FunctionId(106)),
            "invalid_task_sum_inout",
            Signature::with_inout_params([carrier], unit, [0_u32]),
            Effects::NONE,
        )
        .expect("declare invalid sum inout");

    let errors = validate_program(&builder.finish()).expect_err("Task-bearing sum must fail");
    assert!(
        errors
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::InOutShape),
        "{errors:?}"
    );
}

#[test]
fn public_raw_builder_cannot_mint_frontend_proof_certificates() {
    let money = Type::Nominal(TypeId(101), Vec::new());
    let protected = Type::Nominal(TypeId(102), Vec::new());
    let mut builder = ProgramBuilder::new(target());
    let float = builder.type_id(&Type::Float).expect("Float type");
    let integer = builder.type_id(&Type::Int).expect("Int type");
    let unit = builder.type_id(&Type::Unit).expect("Unit type");
    let money_id = builder
        .add_transparent_type(money, &Type::Float)
        .expect("transparent Money type");
    let protected_id = builder
        .add_invariant_record_type(protected, &[Type::Int])
        .expect("protected record");
    let function = builder
        .declare_function(
            Origin::synthetic(FunctionId(0)),
            "raw_proof",
            Signature::new(Vec::new(), unit),
            Effects::NONE,
        )
        .expect("declare function");
    let mut function_builder = builder.function(function).expect("function builder");
    let entry = function_builder.create_block().expect("entry");
    function_builder.set_entry(entry).expect("set entry");
    let raw_float = function_builder
        .append_instruction(
            entry,
            InstructionKind::Constant(Constant::float(10.0)),
            &[float],
            Origin::synthetic(FunctionId(0)),
        )
        .expect("raw float")[0];
    let raw_integer = function_builder
        .append_instruction(
            entry,
            InstructionKind::Constant(Constant::Int(10)),
            &[integer],
            Origin::synthetic(FunctionId(0)),
        )
        .expect("raw integer")[0];

    let refine = function_builder
        .append_instruction(
            entry,
            InstructionKind::RefineProven { value: raw_float },
            &[money_id],
            Origin::synthetic(FunctionId(0)),
        )
        .expect_err("public builder must reject a refinement proof certificate");
    assert_eq!(refine.code(), BuildErrorCode::TrustedInstruction);

    let invariant = function_builder
        .append_instruction(
            entry,
            InstructionKind::InvariantRecordProven {
                fields: Box::from([raw_integer]),
            },
            &[protected_id],
            Origin::synthetic(FunctionId(0)),
        )
        .expect_err("public builder must reject an invariant proof certificate");
    assert_eq!(invariant.code(), BuildErrorCode::TrustedInstruction);
}

#[test]
fn ordinary_product_construction_cannot_forge_an_invariant_record() {
    let protected = Type::Nominal(TypeId(103), Vec::new());
    let mut builder = ProgramBuilder::new(target());
    let integer = builder.type_id(&Type::Int).expect("Int type");
    let protected_id = builder
        .add_invariant_record_type(protected, &[Type::Int])
        .expect("protected record");
    let function = builder
        .declare_function(
            Origin::synthetic(FunctionId(1)),
            "forge",
            Signature::new(Vec::new(), protected_id),
            Effects::NONE,
        )
        .expect("declare function");
    {
        let mut function_builder = builder.function(function).expect("function builder");
        let entry = function_builder.create_block().expect("entry");
        function_builder.set_entry(entry).expect("set entry");
        let field = function_builder
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(1)),
                &[integer],
                Origin::synthetic(FunctionId(1)),
            )
            .expect("field")[0];
        let forged = function_builder
            .append_instruction(
                entry,
                InstructionKind::ProductConstruct {
                    fields: Box::from([field]),
                },
                &[protected_id],
                Origin::synthetic(FunctionId(1)),
            )
            .expect("unchecked forged value")[0];
        function_builder
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Return(forged),
                    Origin::synthetic(FunctionId(1)),
                ),
            )
            .expect("return");
    }
    let errors = validate_program(&builder.finish()).expect_err("forgery must be rejected");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error.message().contains("construction boundary")
    }));
}

#[test]
fn product_insertion_cannot_mutate_an_invariant_record() {
    let protected = Type::Nominal(TypeId(104), Vec::new());
    let mut builder = ProgramBuilder::new(target());
    let integer = builder.type_id(&Type::Int).expect("Int type");
    let protected_id = builder
        .add_invariant_record_type(protected, &[Type::Int])
        .expect("protected record");
    let function = builder
        .declare_function(
            Origin::synthetic(FunctionId(2)),
            "mutate_invariant",
            Signature::new([protected_id, integer], protected_id),
            Effects::NONE,
        )
        .expect("declare function");
    {
        let mut function_builder = builder.function(function).expect("function builder");
        let entry = function_builder.create_block().expect("entry");
        function_builder.set_entry(entry).expect("set entry");
        let record = function_builder
            .append_block_parameter(entry, protected_id)
            .expect("record parameter");
        let replacement = function_builder
            .append_block_parameter(entry, integer)
            .expect("replacement parameter");
        let mutated = function_builder
            .append_instruction(
                entry,
                InstructionKind::ProductInsert {
                    aggregate: record,
                    field: 0,
                    value: replacement,
                },
                &[protected_id],
                Origin::synthetic(FunctionId(2)),
            )
            .expect("unchecked invariant mutation")[0];
        function_builder
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Return(mutated),
                    Origin::synthetic(FunctionId(2)),
                ),
            )
            .expect("return");
    }
    let errors = validate_program(&builder.finish()).expect_err("mutation must be rejected");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error
                .message()
                .contains("invariant-protected semantic value")
    }));
}

#[test]
fn transparent_semantic_chains_obey_the_direct_value_structure_budget() {
    let mut builder = ProgramBuilder::new(target());
    let mut base = Type::Float;
    for index in 0..130_u32 {
        let semantic = Type::Nominal(TypeId(1_000 + index), Vec::new());
        builder
            .add_transparent_type(semantic.clone(), &base)
            .expect("bounded registration itself remains total");
        base = semantic;
    }
    let errors = validate_program(&builder.finish()).expect_err("deep chain must be rejected");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::RepresentationPlan
            && error.message().contains("semantic value closure")
    }));
}

#[test]
fn transparent_chain_and_wide_sum_share_one_semantic_structure_budget() {
    const VARIANTS: usize = 200;
    const TRANSPARENT_LAYERS: u32 = 28;

    let mut builder = ProgramBuilder::new(target());
    let mut base = Type::Nominal(TypeId(2_000), Vec::new());
    builder
        .add_sum_type(base.clone(), &vec![Box::<[Type]>::default(); VARIANTS])
        .expect("each individual sum registration remains total");
    for index in 0..TRANSPARENT_LAYERS {
        let semantic = Type::Nominal(TypeId(2_001 + index), Vec::new());
        builder
            .add_transparent_type(semantic.clone(), &base)
            .expect("each individual transparent registration remains total");
        base = semantic;
    }

    let errors = validate_program(&builder.finish())
        .expect_err("sum variants and transparent layers must consume one shared budget");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::RepresentationPlan
            && error.message().contains("semantic value closure")
    }));
}
