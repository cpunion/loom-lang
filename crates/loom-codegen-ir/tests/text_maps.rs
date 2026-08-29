use loom_codegen_ir::{
    Constant, Effects, InstructionKind, ManagedSafepoint, Origin, ProgramBuilder, Signature,
    TargetLayout, Terminator, TerminatorKind, ValidationCode, ValueDefinition, plan_managed_roots,
    validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

const RESULT_TYPE_ID: TypeId = TypeId(1);
const TEXT_MAP_TYPE_ID: TypeId = TypeId(14);

fn text_map(value: Type) -> Type {
    Type::Nominal(TypeId(100), vec![value])
}

fn option(value: Type) -> Type {
    Type::Nominal(TypeId(101), vec![value])
}

fn canonical_text_map(value: Type) -> Type {
    Type::Nominal(TEXT_MAP_TYPE_ID, vec![value])
}

fn text_map_bulk_types(
    builder: &mut ProgramBuilder,
    value: Type,
) -> (loom_codegen_ir::ValueTypeId, loom_codegen_ir::ValueTypeId) {
    let entry = Type::Tuple(vec![Type::Text, value.clone()]);
    builder
        .add_tuple_type(&[Type::Text, value.clone()])
        .expect("(Text, V)");
    let entries = builder
        .add_managed_list_type(Type::List(Box::new(entry)))
        .expect("List[(Text, V)]");
    let map_semantic = canonical_text_map(value);
    builder
        .add_managed_text_map_type(map_semantic.clone())
        .expect("TextMap[V]");
    let result = builder
        .add_sum_type(
            Type::Nominal(RESULT_TYPE_ID, vec![map_semantic.clone(), Type::Text]),
            &[Box::from([map_semantic]), Box::from([Type::Text])],
        )
        .expect("Result[TextMap[V], Text]");
    (entries, result)
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one checked graph keeps every exact public and compiler-private TextMap result shape together"
)]
fn closed_text_map_instructions_have_exact_value_and_option_shapes() {
    let origin = Origin::synthetic(FunctionId(0));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    builder.add_managed_text_type().expect("managed Text");
    let map = builder
        .add_managed_text_map_type(text_map(Type::Int))
        .expect("TextMap[Int]");
    let option_int = builder
        .add_sum_type(option(Type::Int), &[Box::new([]), Box::from([Type::Int])])
        .expect("Option[Int]");
    let entry_semantic = Type::Tuple(vec![Type::Text, Type::Int]);
    builder
        .add_tuple_type(&[Type::Text, Type::Int])
        .expect("(Text, Int)");
    let option_entry = builder
        .add_sum_type(
            option(entry_semantic.clone()),
            &[Box::new([]), Box::from([entry_semantic])],
        )
        .expect("Option[(Text, Int)]");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let text = builder.type_id(&Type::Text).expect("Text");
    let root = builder
        .declare_function(
            origin,
            "text_map.valid",
            Signature::new([], option_int),
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
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapContains { map: inserted, key },
                &[boolean],
                origin,
            )
            .expect("contains");
        let removed = function
            .append_instruction(
                entry,
                InstructionKind::TextMapRemove { map: inserted, key },
                &[map],
                origin,
            )
            .expect("remove")[0];
        let index = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(0)),
                &[integer],
                origin,
            )
            .expect("index")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapEntryGet {
                    map: removed,
                    index,
                },
                &[option_entry],
                origin,
            )
            .expect("entry read");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::TextMapGet { map: inserted, key },
                &[option_int],
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
#[expect(
    clippy::too_many_lines,
    reason = "one forged graph proves all TextMap operand and result mismatches are rejected together"
)]
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
    builder
        .add_tuple_type(&[Type::Text, Type::Int])
        .expect("(Text, Int)");
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
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapContains {
                    map: empty,
                    key: wrong,
                },
                &[boolean],
                origin,
            )
            .expect("unchecked contains");
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapRemove {
                    map: empty,
                    key: wrong,
                },
                &[boolean],
                origin,
            )
            .expect("unchecked remove");
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapEntryGet {
                    map: empty,
                    index: wrong,
                },
                &[wrong_option],
                origin,
            )
            .expect("unchecked entry read");
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
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.path().contains(".index")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.path().contains(".result[0]")
    }));
}

#[test]
fn text_map_registration_rejects_missing_or_immortal_value_leaves() {
    let mut missing_text = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    missing_text
        .add_managed_text_map_type(text_map(Type::Int))
        .expect("unchecked map without Text keys");
    let errors =
        validate_program(&missing_text.finish()).expect_err("missing managed Text must fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::RepresentationPlan
            && error.message().contains("managed Text keys")
    }));

    let mut immortal_key = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    immortal_key
        .add_immortal_text_type()
        .expect("immortal Text");
    immortal_key
        .add_managed_text_map_type(text_map(Type::Int))
        .expect("unchecked map with immortal keys");
    let errors = validate_program(&immortal_key.finish())
        .expect_err("immortal Text map keys must fail closed");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::RepresentationPlan
            && error.message().contains("managed Text keys")
    }));

    let mut missing_value = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    missing_value
        .add_managed_text_type()
        .expect("managed key Text");
    missing_value
        .add_managed_text_map_type(text_map(Type::Nominal(TypeId(404), Vec::new())))
        .expect("unchecked missing value type");
    let errors =
        validate_program(&missing_value.finish()).expect_err("missing value type must fail");
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

#[test]
fn bulk_construction_has_one_exact_shape_and_roots_its_source_list() {
    let origin = Origin::synthetic(FunctionId(3));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    builder.add_managed_text_type().expect("managed Text");
    let (entries, result) = text_map_bulk_types(&mut builder, Type::Int);
    let root = builder
        .declare_function(
            origin,
            "text_map.bulk.valid",
            Signature::new([entries], result),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    let (source, constructed) = {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let source = function
            .append_block_parameter(entry, entries)
            .expect("entries parameter");
        let constructed = function
            .append_instruction(
                entry,
                InstructionKind::TextMapConstructEntries { entries: source },
                &[result],
                origin,
            )
            .expect("bulk construction")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(constructed), origin),
            )
            .expect("return");
        (source, constructed)
    };
    let program = builder
        .finish_checked()
        .expect("valid TextMap bulk construction");
    let function = program.as_program().function(root).expect("bulk function");
    assert!(function.effects().contains(Effects::MAY_COLLECT));
    let ValueDefinition::InstructionResult { instruction, .. } = function
        .value(constructed)
        .expect("bulk result")
        .definition()
    else {
        panic!("bulk result must be an instruction result")
    };
    let plan = plan_managed_roots(&program, root).expect("bulk root plan");
    assert_eq!(plan.slots().len(), 1, "{plan:?}");
    assert_eq!(plan.slots()[0].value(), source);
    assert!(plan.slots()[0].projection().is_empty());
    assert_eq!(
        plan.state(ManagedSafepoint::Instruction(instruction)),
        Some(1)
    );
}

#[test]
fn bulk_construction_rejects_non_tuple_products_non_text_entries_and_non_result_output() {
    let origin = Origin::synthetic(FunctionId(4));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    builder.add_managed_text_type().expect("managed Text");
    let (entries, result) = text_map_bulk_types(&mut builder, Type::Int);
    let wrong_entry = Type::Tuple(vec![Type::Int, Type::Int]);
    builder
        .add_tuple_type(&[Type::Int, Type::Int])
        .expect("(Int, Int)");
    let wrong_entries = builder
        .add_managed_list_type(Type::List(Box::new(wrong_entry)))
        .expect("List[(Int, Int)]");
    let record_entry = Type::Nominal(TypeId(200), Vec::new());
    builder
        .add_pod_record_type(record_entry.clone(), &[Type::Text, Type::Int])
        .expect("record with the tuple's physical fields");
    let record_entries = builder
        .add_managed_list_type(Type::List(Box::new(record_entry)))
        .expect("List[record]");
    let text = builder.type_id(&Type::Text).expect("Text");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let root = builder
        .declare_function(
            origin,
            "text_map.bulk.invalid",
            Signature::new([entries, wrong_entries, record_entries], unit),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let valid = function
            .append_block_parameter(entry, entries)
            .expect("valid entries");
        let invalid = function
            .append_block_parameter(entry, wrong_entries)
            .expect("invalid entries");
        let forged_product = function
            .append_block_parameter(entry, record_entries)
            .expect("record entries");
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapConstructEntries { entries: invalid },
                &[result],
                origin,
            )
            .expect("unchecked invalid entries");
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapConstructEntries {
                    entries: forged_product,
                },
                &[result],
                origin,
            )
            .expect("unchecked record entries");
        function
            .append_instruction(
                entry,
                InstructionKind::TextMapConstructEntries { entries: valid },
                &[text],
                origin,
            )
            .expect("unchecked invalid result");
        let returned = function
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
                Terminator::new(TerminatorKind::Return(returned), origin),
            )
            .expect("return");
    }
    let errors = validate_program(&builder.finish()).expect_err("malformed bulk operations fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.path().contains(".entries")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.path().contains(".result[0]")
    }));
    assert!(
        errors
            .as_slice()
            .iter()
            .filter(|error| {
                error.code() == ValidationCode::TypeMismatch && error.path().contains(".entries")
            })
            .count()
            >= 2,
        "{errors:#?}"
    );
}

#[test]
fn functional_remove_roots_only_its_exact_map_source_at_the_collecting_site() {
    let origin = Origin::synthetic(FunctionId(2));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    builder.add_managed_text_type().expect("managed Text");
    let map = builder
        .add_managed_text_map_type(text_map(Type::Int))
        .expect("TextMap[Int]");
    let text = builder.type_id(&Type::Text).expect("Text");
    let root = builder
        .declare_function(
            origin,
            "text_map.remove.roots",
            Signature::new([map, text], map),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    let removed = {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let source = function
            .append_block_parameter(entry, map)
            .expect("map parameter");
        let key = function
            .append_block_parameter(entry, text)
            .expect("key parameter");
        let removed = function
            .append_instruction(
                entry,
                InstructionKind::TextMapRemove { map: source, key },
                &[map],
                origin,
            )
            .expect("remove")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(removed), origin),
            )
            .expect("return");
        (source, key, removed)
    };
    let program = builder
        .finish_checked()
        .expect("checked TextMap.remove LCIR");
    let plan = plan_managed_roots(&program, root).expect("remove root plan");
    assert_eq!(plan.slots().len(), 1, "{plan:?}");
    assert_eq!(plan.slots()[0].value(), removed.0);
    assert!(plan.slots()[0].projection().is_empty());
    assert!(!plan.slots().iter().any(|slot| slot.value() == removed.1));
    let ValueDefinition::InstructionResult { instruction, .. } = program
        .as_program()
        .function(root)
        .and_then(|function| function.value(removed.2))
        .expect("remove result")
        .definition()
    else {
        panic!("remove result must be an instruction result")
    };
    assert_eq!(
        plan.state(ManagedSafepoint::Instruction(instruction)),
        Some(1)
    );
    assert!(
        program
            .as_program()
            .function(root)
            .expect("remove function")
            .effects()
            .contains(Effects::MAY_COLLECT)
    );
}
