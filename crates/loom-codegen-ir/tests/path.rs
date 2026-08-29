use loom_codegen_ir::{
    Constant, Effects, InstructionKind, ManagedRootProjection, ManagedSafepoint, Origin,
    PATH_TYPE_ID, ProgramBuilder, Signature, TargetLayout, Terminator, TerminatorKind,
    ValidationCode, ValueDefinition, ValueTypeId, ValueTypeKind, dump_program, plan_managed_roots,
};
use loom_mir::{FunctionId as MirFunctionId, Type, TypeId};

const RESULT_TYPE_ID: TypeId = TypeId(1);
const PATH_ERROR_TYPE_ID: TypeId = TypeId(13);

#[derive(Clone, Copy)]
struct PathTypes {
    unit: ValueTypeId,
    text: ValueTypeId,
    path: ValueTypeId,
    result: ValueTypeId,
}

fn origin() -> Origin {
    Origin::synthetic(MirFunctionId(0))
}

fn builder_with_path_types() -> (ProgramBuilder, PathTypes) {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let text = builder
        .add_managed_text_type()
        .expect("canonical managed Text");
    let path_semantic = Type::Nominal(PATH_TYPE_ID, Vec::new());
    let path = builder
        .add_invariant_record_type(path_semantic.clone(), &[Type::Text])
        .expect("canonical Path");
    let error_semantic = Type::Nominal(PATH_ERROR_TYPE_ID, Vec::new());
    builder
        .add_sum_type(error_semantic.clone(), &[Box::new([]), Box::new([])])
        .expect("canonical PathError");
    let result = builder
        .add_sum_type(
            Type::Nominal(
                RESULT_TYPE_ID,
                vec![path_semantic.clone(), error_semantic.clone()],
            ),
            &[Box::from([path_semantic]), Box::from([error_semantic])],
        )
        .expect("Result[Path, PathError]");
    (
        builder,
        PathTypes {
            unit,
            text,
            path,
            result,
        },
    )
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one manual function keeps all three Path opcodes and their exact root contract reviewable"
)]
fn checked_path_instructions_have_exact_effects_live_after_roots_and_dump() {
    let (mut builder, types) = builder_with_path_types();
    let root = builder
        .declare_function(
            origin(),
            "path",
            Signature::new([types.text, types.path, types.path], types.unit),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    let (from_result, join_result, as_text_result, base, child) = {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let text = function
            .append_block_parameter(entry, types.text)
            .expect("Text parameter");
        let base = function
            .append_block_parameter(entry, types.path)
            .expect("base Path parameter");
        let child = function
            .append_block_parameter(entry, types.path)
            .expect("child Path parameter");
        let from = function
            .append_instruction(
                entry,
                InstructionKind::PathFromText {
                    text,
                    ok_variant: 0,
                    error_variant: 1,
                    contains_nul_variant: 0,
                },
                &[types.result],
                origin(),
            )
            .expect("Path.from_text")[0];
        let joined = function
            .append_instruction(
                entry,
                InstructionKind::PathJoin {
                    base,
                    child,
                    ok_variant: 0,
                    error_variant: 1,
                    absolute_join_variant: 1,
                },
                &[types.result],
                origin(),
            )
            .expect("Path.join")[0];
        let rendered = function
            .append_instruction(
                entry,
                InstructionKind::PathAsText { path: base },
                &[types.text],
                origin(),
            )
            .expect("Path.as_text")[0];
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[types.unit],
                origin(),
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin()),
            )
            .expect("return");
        (from, joined, rendered, base, child)
    };

    let program = builder.finish_checked().expect("checked Path program");
    assert_eq!(
        program
            .as_program()
            .representations()
            .value_type(types.path)
            .expect("canonical Path value type")
            .kind(),
        ValueTypeKind::InvariantProduct
    );
    let function = program.as_program().function(root).expect("function");
    assert!(function.effects().contains(Effects::MAY_COLLECT));
    let instruction = |value| match function.value(value).expect("value").definition() {
        ValueDefinition::InstructionResult { instruction, .. } => instruction,
        ValueDefinition::BlockParameter { .. } => panic!("expected instruction result"),
    };
    let from = instruction(from_result);
    let join = instruction(join_result);
    let as_text = instruction(as_text_result);
    let roots = plan_managed_roots(&program, root).expect("managed Path roots");
    assert!(
        roots.state(ManagedSafepoint::Instruction(from)).is_none(),
        "Path.from_text must not collect"
    );
    assert!(
        roots
            .state(ManagedSafepoint::Instruction(as_text))
            .is_none(),
        "Path.as_text must not collect"
    );
    let state = roots
        .state(ManagedSafepoint::Instruction(join))
        .expect("Path.join safepoint");
    let base_slot = roots
        .slots()
        .iter()
        .position(|slot| {
            slot.value() == base && slot.projection() == [ManagedRootProjection::ProductField(0)]
        })
        .expect("base Path is live after join");
    assert!(
        roots.slots().iter().all(|slot| slot.value() != child),
        "dead child Path must not be rooted merely because join consumes it"
    );
    let state = usize::try_from(state).expect("root state");
    let word = roots.bitmaps()[state * roots.bitmap_words() + base_slot / 64];
    assert_ne!(word & (1_u64 << (base_slot % 64)), 0);

    let dump = dump_program(&program);
    for opcode in ["path.from_text", "path.as_text", "path.join"] {
        assert!(dump.contains(opcode), "missing {opcode}: {dump}");
    }
}

#[test]
fn ordinary_product_opcodes_cannot_forge_or_rewrite_path() {
    let (mut builder, types) = builder_with_path_types();
    let root = builder
        .declare_function(
            origin(),
            "forge_path",
            Signature::new([types.text, types.path], types.unit),
            Effects::NONE,
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let text = function
            .append_block_parameter(entry, types.text)
            .expect("Text parameter");
        let path = function
            .append_block_parameter(entry, types.path)
            .expect("Path parameter");
        function
            .append_instruction(
                entry,
                InstructionKind::ProductConstruct {
                    fields: Box::from([text]),
                },
                &[types.path],
                origin(),
            )
            .expect("unchecked Path construction");
        function
            .append_instruction(
                entry,
                InstructionKind::ProductInsert {
                    aggregate: path,
                    field: 0,
                    value: text,
                },
                &[types.path],
                origin(),
            )
            .expect("unchecked Path rewrite");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[types.unit],
                origin(),
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(unit), origin()),
            )
            .expect("return");
    }

    let errors = builder
        .finish_checked()
        .expect_err("ordinary product operations must not establish the Path invariant");
    for message in [
        "product construction opcode does not match the result type's checked construction boundary",
        "product insertion cannot mutate a transparent or invariant-protected semantic value",
    ] {
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code() == ValidationCode::TypeMismatch
                    && error.message().contains(message)),
            "missing `{message}`: {errors:#?}"
        );
    }
}

#[test]
fn path_opcodes_reject_a_transparent_path_error_carrier() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let text = builder
        .add_managed_text_type()
        .expect("canonical managed Text");
    let path_semantic = Type::Nominal(PATH_TYPE_ID, Vec::new());
    builder
        .add_invariant_record_type(path_semantic.clone(), &[Type::Text])
        .expect("canonical Path");
    let dummy_error = Type::Nominal(TypeId(90), Vec::new());
    builder
        .add_sum_type(dummy_error.clone(), &[Box::new([]), Box::new([])])
        .expect("dummy closed error");
    let path_error = Type::Nominal(PATH_ERROR_TYPE_ID, Vec::new());
    builder
        .add_transparent_type(path_error.clone(), &dummy_error)
        .expect("layout-compatible but noncanonical PathError");
    let result = builder
        .add_sum_type(
            Type::Nominal(
                RESULT_TYPE_ID,
                vec![path_semantic.clone(), path_error.clone()],
            ),
            &[Box::from([path_semantic]), Box::from([path_error])],
        )
        .expect("Result[Path, PathError]");
    let root = builder
        .declare_function(
            origin(),
            "transparent_path_error",
            Signature::new([text], unit),
            Effects::NONE,
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let source = function
            .append_block_parameter(entry, text)
            .expect("Text parameter");
        function
            .append_instruction(
                entry,
                InstructionKind::PathFromText {
                    text: source,
                    ok_variant: 0,
                    error_variant: 1,
                    contains_nul_variant: 0,
                },
                &[result],
                origin(),
            )
            .expect("Path.from_text");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(),
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(unit), origin()),
            )
            .expect("return");
    }

    let errors = builder
        .finish_checked()
        .expect_err("PathError must not reuse a transparent representation boundary");
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstructionShape
                && error.message().contains("canonical direct PathError#13")
        }),
        "{errors:#?}"
    );
}

#[test]
fn independent_validation_rejects_wrong_path_operands_variants_and_effects() {
    let (mut builder, types) = builder_with_path_types();
    let root = builder
        .declare_function(
            origin(),
            "invalid_path",
            Signature::new([types.text, types.path], types.unit),
            Effects::NONE,
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let text = function
            .append_block_parameter(entry, types.text)
            .expect("Text parameter");
        let path = function
            .append_block_parameter(entry, types.path)
            .expect("Path parameter");
        function
            .append_instruction(
                entry,
                InstructionKind::PathFromText {
                    text: path,
                    ok_variant: 1,
                    error_variant: 0,
                    contains_nul_variant: 1,
                },
                &[types.result],
                origin(),
            )
            .expect("malformed from_text");
        function
            .append_instruction(
                entry,
                InstructionKind::PathAsText { path: text },
                &[types.path],
                origin(),
            )
            .expect("malformed as_text");
        function
            .append_instruction(
                entry,
                InstructionKind::PathJoin {
                    base: text,
                    child: path,
                    ok_variant: 1,
                    error_variant: 0,
                    absolute_join_variant: 0,
                },
                &[types.result],
                origin(),
            )
            .expect("malformed join");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[types.unit],
                origin(),
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin()),
            )
            .expect("return");
    }
    let errors = builder
        .finish_checked()
        .expect_err("malformed Path program must fail closed");
    for code in [
        ValidationCode::TypeMismatch,
        ValidationCode::InstructionShape,
        ValidationCode::EffectMismatch,
    ] {
        assert!(
            errors.as_slice().iter().any(|error| error.code() == code),
            "missing {code:?}: {errors:#?}"
        );
    }
}
