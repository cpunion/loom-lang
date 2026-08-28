use loom_codegen_ir::{
    Effects, InstructionKind, ManagedSafepoint, Origin, ProgramBuilder, Signature, TargetLayout,
    Terminator, TerminatorKind, ValidationCode, ValueDefinition, dump_program, plan_managed_roots,
    validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

const RESULT_TYPE: TypeId = TypeId(1);
const TEXT_MAP_TYPE: TypeId = TypeId(15);
const JSON_TYPE: TypeId = TypeId(16);
const JSON_ERROR_TYPE: TypeId = TypeId(17);

struct JsonTypes {
    json: loom_codegen_ir::ValueTypeId,
    result: loom_codegen_ir::ValueTypeId,
}

fn nominal(id: TypeId) -> Type {
    Type::Nominal(id, Vec::new())
}

fn add_json_types(builder: &mut ProgramBuilder) -> JsonTypes {
    builder
        .add_managed_text_type()
        .expect("canonical managed Text");
    let json_semantic = nominal(JSON_TYPE);
    let list_semantic = Type::List(Box::new(json_semantic.clone()));
    let map_semantic = Type::Nominal(TEXT_MAP_TYPE, vec![json_semantic.clone()]);
    builder
        .add_managed_list_type(list_semantic.clone())
        .expect("List[Json] cycle breaker");
    builder
        .add_managed_text_map_type(map_semantic.clone())
        .expect("TextMap[Json] cycle breaker");
    let json = builder
        .add_sum_type(
            json_semantic,
            &[
                Box::new([]),
                Box::from([Type::Bool]),
                Box::from([Type::Float]),
                Box::from([Type::Text]),
                Box::from([list_semantic]),
                Box::from([map_semantic]),
            ],
        )
        .expect("canonical recursive Json");
    let error_semantic = nominal(JSON_ERROR_TYPE);
    builder
        .add_sum_type(
            error_semantic.clone(),
            &[
                Box::from([Type::Int]),
                Box::from([Type::Int]),
                Box::new([]),
                Box::new([]),
            ],
        )
        .expect("JsonError");
    let result = builder
        .add_sum_type(
            Type::Nominal(RESULT_TYPE, vec![Type::Text, error_semantic.clone()]),
            &[Box::from([Type::Text]), Box::from([error_semantic])],
        )
        .expect("Result[Text, JsonError]");
    JsonTypes { json, result }
}

#[test]
fn canonical_json_format_is_collecting_dumpable_and_rooted() {
    let origin = Origin::synthetic(FunctionId(200));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let types = add_json_types(&mut builder);
    let root = builder
        .declare_function(
            origin,
            "json.format.valid",
            Signature::new([types.json], types.json),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    let (json, formatted) = {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let json = function
            .append_block_parameter(entry, types.json)
            .expect("Json parameter");
        let formatted = function
            .append_instruction(
                entry,
                InstructionKind::JsonFormat {
                    json,
                    ok_variant: 0,
                    error_variant: 1,
                    depth_limit_variant: 2,
                    non_finite_number_variant: 3,
                },
                &[types.result],
                origin,
            )
            .expect("format Json")[0];
        function
            .terminate(entry, Terminator::new(TerminatorKind::Return(json), origin))
            .expect("return live Json");
        (json, formatted)
    };
    let checked = builder
        .finish_checked()
        .expect("canonical JSON formatter graph");
    let dump = dump_program(&checked);
    assert!(
        dump.contains("json.format %v0, ok 0, error 1, depth_limit 2, non_finite_number 3"),
        "{dump}"
    );

    let plan = plan_managed_roots(&checked, root).expect("managed-root plan");
    assert!(plan.slots().iter().any(|slot| slot.value() == json));
    let ValueDefinition::InstructionResult { instruction, .. } = checked
        .as_program()
        .function(root)
        .and_then(|function| function.value(formatted))
        .expect("format result")
        .definition()
    else {
        panic!("format result must be an instruction result")
    };
    let state = plan
        .state(ManagedSafepoint::Instruction(instruction))
        .expect("JsonFormat collecting state");
    assert_ne!(state, 0);
    let state = usize::try_from(state).expect("root state index");
    let start = state * plan.bitmap_words();
    assert!(
        plan.bitmaps()[start..start + plan.bitmap_words()]
            .iter()
            .any(|word| *word != 0)
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one hostile graph exercises nominal identity, recursive shape, nested sum, and selector forgery together"
)]
fn independent_validation_rejects_forged_json_format_shapes_and_mappings() {
    let origin = Origin::synthetic(FunctionId(201));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let types = add_json_types(&mut builder);
    let wrong_json_semantic = nominal(TypeId(204));
    let wrong_json = builder
        .add_sum_type(
            wrong_json_semantic,
            &[
                Box::new([]),
                Box::from([Type::Bool]),
                Box::from([Type::Float]),
                Box::from([Type::Text]),
                Box::from([Type::List(Box::new(nominal(JSON_TYPE)))]),
                Box::from([Type::Nominal(TEXT_MAP_TYPE, vec![nominal(JSON_TYPE)])]),
            ],
        )
        .expect("nonrecursive Json-shaped sum");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let root = builder
        .declare_function(
            origin,
            "json.format.forged",
            Signature::new([types.json, wrong_json], unit),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let json = function
            .append_block_parameter(entry, types.json)
            .expect("Json parameter");
        let forged_json = function
            .append_block_parameter(entry, wrong_json)
            .expect("forged Json parameter");
        function
            .append_instruction(
                entry,
                InstructionKind::JsonFormat {
                    json: forged_json,
                    ok_variant: 0,
                    error_variant: 1,
                    depth_limit_variant: 2,
                    non_finite_number_variant: 3,
                },
                &[types.result],
                origin,
            )
            .expect("forged recursive shape");
        function
            .append_instruction(
                entry,
                InstructionKind::JsonFormat {
                    json,
                    ok_variant: 0,
                    error_variant: 0,
                    depth_limit_variant: 2,
                    non_finite_number_variant: 3,
                },
                &[types.result],
                origin,
            )
            .expect("forged Result variant mappings");
        function
            .append_instruction(
                entry,
                InstructionKind::JsonFormat {
                    json,
                    ok_variant: 0,
                    error_variant: 1,
                    depth_limit_variant: 2,
                    non_finite_number_variant: 2,
                },
                &[types.result],
                origin,
            )
            .expect("forged JsonError variant mappings");
        function
            .append_instruction(
                entry,
                InstructionKind::JsonFormat {
                    json,
                    ok_variant: 0,
                    error_variant: 1,
                    depth_limit_variant: 3,
                    non_finite_number_variant: 2,
                },
                &[types.result],
                origin,
            )
            .expect("swapped JsonError variant mappings");
        function
            .append_instruction(
                entry,
                InstructionKind::JsonFormat {
                    json,
                    ok_variant: 0,
                    error_variant: 1,
                    depth_limit_variant: 2,
                    non_finite_number_variant: 3,
                },
                &[types.json],
                origin,
            )
            .expect("forged result");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(loom_codegen_ir::Constant::Unit),
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

    let errors = validate_program(&builder.finish()).expect_err("forged JSON format must fail");
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::TypeMismatch
                && error.path().contains("instruction[0].json")
        }),
        "{errors:?}"
    );
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstructionShape
                && error.path().contains("instruction[1].variants")
        }),
        "{errors:?}"
    );
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstructionShape
                && error.path().contains("instruction[2].error_variants")
        }),
        "{errors:?}"
    );
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::TypeMismatch
                && error.path().contains("instruction[4].result[0]")
        }),
        "{errors:?}"
    );
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstructionShape
                && error.path().contains("instruction[3].error_variants")
        }),
        "{errors:?}"
    );
}

#[test]
fn json_format_requires_the_collecting_effect() {
    let origin = Origin::synthetic(FunctionId(202));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let types = add_json_types(&mut builder);
    let root = builder
        .declare_function(
            origin,
            "json.format.missing.effect",
            Signature::new([types.json], types.json),
            Effects::NONE,
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let json = function
            .append_block_parameter(entry, types.json)
            .expect("Json parameter");
        function
            .append_instruction(
                entry,
                InstructionKind::JsonFormat {
                    json,
                    ok_variant: 0,
                    error_variant: 1,
                    depth_limit_variant: 2,
                    non_finite_number_variant: 3,
                },
                &[types.result],
                origin,
            )
            .expect("format Json");
        function
            .terminate(entry, Terminator::new(TerminatorKind::Return(json), origin))
            .expect("return");
    }

    let errors = validate_program(&builder.finish()).expect_err("missing MAY_COLLECT must fail");
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::EffectMismatch
                && error.message().contains("may_collect")
        }),
        "{errors:?}"
    );
}
