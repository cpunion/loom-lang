use loom_codegen_ir::{
    Constant, Effects, InstructionKind, Origin, ProgramBuilder, Signature, TargetLayout,
    Terminator, TerminatorKind, ValidationCode, ValueTypeKind, dump_program, validate_program,
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

    let function = builder
        .declare_function(
            Origin::synthetic(FunctionId(0)),
            "round_trip",
            Signature::new(Vec::new(), float),
            Effects::NONE,
        )
        .expect("declare function");
    {
        let mut function_builder = builder.function(function).expect("function builder");
        let entry = function_builder.create_block().expect("entry");
        function_builder.set_entry(entry).expect("set entry");
        let raw = function_builder
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::float(10.0)),
                &[float],
                Origin::synthetic(FunctionId(0)),
            )
            .expect("raw value")[0];
        let established = function_builder
            .append_instruction(
                entry,
                InstructionKind::RefineProven { value: raw },
                &[money_id],
                Origin::synthetic(FunctionId(0)),
            )
            .expect("established value")[0];
        let widened = function_builder
            .append_instruction(
                entry,
                InstructionKind::Unrefine { value: established },
                &[float],
                Origin::synthetic(FunctionId(0)),
            )
            .expect("widened value")[0];
        function_builder
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Return(widened),
                    Origin::synthetic(FunctionId(0)),
                ),
            )
            .expect("return");
    }
    let checked = builder.finish_checked().expect("checked semantic casts");
    let dump = dump_program(&checked);
    assert!(dump.contains("transparent(t4)"), "{dump}");
    assert!(dump.contains("refine.proven"), "{dump}");
    assert!(dump.contains("unrefine"), "{dump}");
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
fn proven_refinement_requires_the_exact_declared_base_type() {
    let money = Type::Nominal(TypeId(12), Vec::new());
    let mut builder = ProgramBuilder::new(target());
    let integer = builder.type_id(&Type::Int).expect("Int type");
    let money_id = builder
        .add_transparent_type(money, &Type::Float)
        .expect("transparent Money type");
    let function = builder
        .declare_function(
            Origin::synthetic(FunctionId(2)),
            "wrong_base",
            Signature::new(Vec::new(), money_id),
            Effects::NONE,
        )
        .expect("declare function");
    {
        let mut function_builder = builder.function(function).expect("function builder");
        let entry = function_builder.create_block().expect("entry");
        function_builder.set_entry(entry).expect("set entry");
        let raw = function_builder
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(10)),
                &[integer],
                Origin::synthetic(FunctionId(2)),
            )
            .expect("wrong raw value")[0];
        let forged = function_builder
            .append_instruction(
                entry,
                InstructionKind::RefineProven { value: raw },
                &[money_id],
                Origin::synthetic(FunctionId(2)),
            )
            .expect("unchecked refinement")[0];
        function_builder
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Return(forged),
                    Origin::synthetic(FunctionId(2)),
                ),
            )
            .expect("return");
    }
    let errors = validate_program(&builder.finish()).expect_err("wrong base must be rejected");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error.message().contains("exact declared base")
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
