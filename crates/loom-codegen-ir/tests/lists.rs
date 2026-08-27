use loom_codegen_ir::{
    Constant, Effects, InstructionKind, LIST_LITERAL_MAX_ELEMENTS, Origin, ProgramBuilder,
    Signature, TargetLayout, Terminator, TerminatorKind, ValidationCode, validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

fn option(element: Type) -> Type {
    Type::Nominal(TypeId(100), vec![element])
}

#[test]
fn concrete_list_instructions_have_one_closed_typed_shape() {
    let origin = Origin::synthetic(FunctionId(0));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let list_semantic = Type::List(Box::new(Type::Int));
    let list = builder
        .add_managed_list_type(list_semantic)
        .expect("List[Int]");
    let option = builder
        .add_sum_type(option(Type::Int), &[Box::new([]), Box::from([Type::Int])])
        .expect("Option[Int]");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let root = builder
        .declare_function(
            origin,
            "lists.valid",
            Signature::new([], option),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(7)),
                &[integer],
                origin,
            )
            .expect("integer")[0];
        let values = function
            .append_instruction(
                entry,
                InstructionKind::ListConstruct {
                    elements: Box::new([value]),
                },
                &[list],
                origin,
            )
            .expect("list")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::ListLength { list: values },
                &[integer],
                origin,
            )
            .expect("length");
        let appended = function
            .append_instruction(
                entry,
                InstructionKind::ListAppend {
                    list: values,
                    value,
                },
                &[list],
                origin,
            )
            .expect("append")[0];
        let result = function
            .append_instruction(
                entry,
                InstructionKind::ListGet {
                    list: appended,
                    index: value,
                },
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
    builder.finish_checked().expect("valid typed List program");
}

#[test]
fn malformed_list_operands_option_shape_and_literal_budget_are_rejected() {
    let origin = Origin::synthetic(FunctionId(1));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let list = builder
        .add_managed_list_type(Type::List(Box::new(Type::Int)))
        .expect("List[Int]");
    let wrong_option = builder
        .add_sum_type(option(Type::Bool), &[Box::new([]), Box::from([Type::Bool])])
        .expect("Option[Bool]");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let root = builder
        .declare_function(
            origin,
            "lists.invalid",
            Signature::new([], unit),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let wrong = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[boolean],
                origin,
            )
            .expect("Bool")[0];
        let values = function
            .append_instruction(
                entry,
                InstructionKind::ListConstruct {
                    elements: vec![wrong; LIST_LITERAL_MAX_ELEMENTS + 1].into_boxed_slice(),
                },
                &[list],
                origin,
            )
            .expect("unchecked List")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::ListAppend {
                    list: values,
                    value: wrong,
                },
                &[list],
                origin,
            )
            .expect("unchecked append");
        function
            .append_instruction(
                entry,
                InstructionKind::ListGet {
                    list: values,
                    index: wrong,
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
    let errors = validate_program(&builder.finish()).expect_err("malformed Lists must fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape && error.path().contains("elements")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.path().contains("element[0]")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.path().contains(".index")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.message().contains("Option payload")
    }));
}

#[test]
fn list_element_registration_fails_closed_for_missing_or_immortal_text() {
    let mut missing = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    missing
        .add_managed_list_type(Type::List(Box::new(Type::Text)))
        .expect("unchecked List[Text]");
    let errors = validate_program(&missing.finish()).expect_err("missing Text must fail");
    assert!(
        errors
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::RepresentationPlan)
    );

    let mut immortal = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    immortal.add_immortal_text_type().expect("immortal Text");
    immortal
        .add_managed_list_type(Type::List(Box::new(Type::Text)))
        .expect("unchecked List[Text]");
    let errors = validate_program(&immortal.finish()).expect_err("immortal element must fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::RepresentationPlan
            && error.message().contains("concrete closed List")
    }));
}
