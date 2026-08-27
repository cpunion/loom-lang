use loom_codegen_ir::{
    Constant, Effects, InstructionKind, Origin, ProgramBuilder, Signature, TargetLayout,
    Terminator, TerminatorKind, ValidationCode, validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

fn text_map(value: Type) -> Type {
    Type::Nominal(TypeId(100), vec![value])
}

fn option(value: Type) -> Type {
    Type::Nominal(TypeId(101), vec![value])
}

#[test]
fn closed_text_map_instructions_have_exact_value_and_option_shapes() {
    let origin = Origin::synthetic(FunctionId(0));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    builder.add_managed_text_type().expect("managed Text");
    let map = builder
        .add_managed_text_map_type(text_map(Type::Int))
        .expect("TextMap[Int]");
    let option = builder
        .add_sum_type(option(Type::Int), &[Box::new([]), Box::from([Type::Int])])
        .expect("Option[Int]");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let text = builder.type_id(&Type::Text).expect("Text");
    let root = builder
        .declare_function(
            origin,
            "text_map.valid",
            Signature::new([], option),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let empty = function
            .append_instruction(entry, InstructionKind::TextMapConstruct, &[map], origin)
            .expect("empty map")[0];
        let key = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral { utf8: "key".into() },
                &[text],
                origin,
            )
            .expect("key")[0];
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(7)),
                &[integer],
                origin,
            )
            .expect("value")[0];
        let inserted = function
            .append_instruction(
                entry,
                InstructionKind::TextMapInsert {
                    map: empty,
                    key,
                    value,
                },
                &[map],
                origin,
            )
            .expect("insert")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapLength { map: inserted },
                &[integer],
                origin,
            )
            .expect("length");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::TextMapGet { map: inserted, key },
                &[option],
                origin,
            )
            .expect("get")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin),
            )
            .expect("return");
    }
    builder
        .finish_checked()
        .expect("valid typed TextMap program");
}

#[test]
fn malformed_text_map_key_value_and_option_are_rejected_together() {
    let origin = Origin::synthetic(FunctionId(1));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    builder.add_managed_text_type().expect("managed Text");
    let map = builder
        .add_managed_text_map_type(text_map(Type::Int))
        .expect("TextMap[Int]");
    let wrong_option = builder
        .add_sum_type(option(Type::Bool), &[Box::new([]), Box::from([Type::Bool])])
        .expect("Option[Bool]");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let root = builder
        .declare_function(
            origin,
            "text_map.invalid",
            Signature::new([], unit),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let empty = function
            .append_instruction(entry, InstructionKind::TextMapConstruct, &[map], origin)
            .expect("empty map")[0];
        let wrong = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[boolean],
                origin,
            )
            .expect("Bool")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapInsert {
                    map: empty,
                    key: wrong,
                    value: wrong,
                },
                &[map],
                origin,
            )
            .expect("unchecked insert");
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapGet {
                    map: empty,
                    key: wrong,
                },
                &[wrong_option],
                origin,
            )
            .expect("unchecked get");
        let result = function
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
                Terminator::new(TerminatorKind::Return(result), origin),
            )
            .expect("return");
    }
    let errors = validate_program(&builder.finish()).expect_err("malformed TextMap must fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.path().contains(".key")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.path().contains(".value")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.message().contains("Option payload")
    }));
}

#[test]
fn text_map_registration_rejects_missing_or_immortal_value_leaves() {
    let mut missing = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    missing.add_managed_text_type().expect("managed key Text");
    missing
        .add_managed_text_map_type(text_map(Type::Nominal(TypeId(404), Vec::new())))
        .expect("unchecked missing value type");
    let errors = validate_program(&missing.finish()).expect_err("missing value type must fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::RepresentationPlan
            && error.message().contains("closed TextMap")
    }));

    let mut immortal = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    immortal.add_immortal_text_type().expect("immortal Text");
    immortal
        .add_managed_text_map_type(text_map(Type::Text))
        .expect("unchecked TextMap[Text]");
    let errors =
        validate_program(&immortal.finish()).expect_err("immortal map value must fail closed");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::RepresentationPlan
            && error.message().contains("closed TextMap")
    }));
}
