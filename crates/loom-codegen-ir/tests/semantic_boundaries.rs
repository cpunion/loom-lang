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
    let money = Type::Nominal(TypeId(10), Vec::new());
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
fn public_raw_builder_cannot_mint_frontend_proof_certificates() {
    let money = Type::Nominal(TypeId(12), Vec::new());
    let protected = Type::Nominal(TypeId(13), Vec::new());
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
    let protected = Type::Nominal(TypeId(11), Vec::new());
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
