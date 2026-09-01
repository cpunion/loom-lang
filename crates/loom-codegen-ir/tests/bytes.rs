use loom_codegen_ir::{
    BlockTarget, BoolPredicate, BuildErrorCode, CanonicalTypeCatalog, Constant, Effects,
    InstructionKind, IntPredicate, ManagedSafepoint, Origin, ProgramBuilder, Signature,
    TargetLayout, Terminator, TerminatorKind, ValidationCode, ValueTypeId, dump_program,
    plan_managed_roots,
};
use loom_mir::{FunctionId as MirFunctionId, Type, TypeId};

const OPTION_TYPE_ID: TypeId = TypeId(100);
const RESULT_TYPE_ID: TypeId = TypeId(101);
const BYTES_TYPE_ID: TypeId = TypeId(109);
const DECODE_TEXT_ERROR_TYPE_ID: TypeId = TypeId(111);

fn bytes_catalog() -> CanonicalTypeCatalog {
    CanonicalTypeCatalog {
        result: Some(RESULT_TYPE_ID),
        option: Some(OPTION_TYPE_ID),
        bytes: Some(BYTES_TYPE_ID),
        decode_text_error: Some(DECODE_TEXT_ERROR_TYPE_ID),
        ..CanonicalTypeCatalog::default()
    }
}

#[derive(Clone, Copy)]
struct BytesTypes {
    unit: ValueTypeId,
    boolean: ValueTypeId,
    integer: ValueTypeId,
    text: ValueTypeId,
    bytes: ValueTypeId,
    option_int: ValueTypeId,
    decode_result: ValueTypeId,
}

fn origin() -> Origin {
    Origin::synthetic(MirFunctionId(0))
}

fn builder_with_bytes_types() -> (ProgramBuilder, BytesTypes) {
    let mut builder = ProgramBuilder::with_canonical_types(
        TargetLayout::new(64).expect("target"),
        bytes_catalog(),
    );
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let text = builder
        .add_managed_text_type()
        .expect("canonical managed Text");
    let bytes = builder
        .add_managed_bytes_type(Type::Nominal(BYTES_TYPE_ID, Vec::new()))
        .expect("canonical managed Bytes");
    let option_int = builder
        .add_sum_type(
            Type::Nominal(OPTION_TYPE_ID, vec![Type::Int]),
            &[Box::new([]), Box::new([Type::Int])],
        )
        .expect("Option[Int]");
    let decode_error_semantic = Type::Nominal(DECODE_TEXT_ERROR_TYPE_ID, Vec::new());
    builder
        .add_sum_type(decode_error_semantic.clone(), &[Box::new([])])
        .expect("DecodeTextError");
    let decode_result = builder
        .add_sum_type(
            Type::Nominal(
                RESULT_TYPE_ID,
                vec![Type::Text, decode_error_semantic.clone()],
            ),
            &[Box::new([Type::Text]), Box::from([decode_error_semantic])],
        )
        .expect("Result[Text, DecodeTextError]");
    (
        builder,
        BytesTypes {
            unit,
            boolean,
            integer,
            text,
            bytes,
            option_int,
            decode_result,
        },
    )
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one manual function keeps the complete Bytes opcode and safepoint contract reviewable"
)]
fn checked_bytes_instruction_family_has_exact_shapes_effects_roots_and_dump() {
    let (mut builder, types) = builder_with_bytes_types();
    let root = builder
        .declare_function(
            origin(),
            "bytes",
            Signature::new(
                [types.text, types.bytes, types.bytes, types.integer],
                types.unit,
            ),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    let (append_id, push_id, decode_id, left_id) = {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        let lower_checked = function.create_block().expect("lower checked");
        let lower_failed = function.create_block().expect("lower failed");
        let push_block = function.create_block().expect("push block");
        let upper_failed = function.create_block().expect("upper failed");
        function.set_entry(entry).expect("set entry");
        let text = function
            .append_block_parameter(entry, types.text)
            .expect("Text parameter");
        let left = function
            .append_block_parameter(entry, types.bytes)
            .expect("left Bytes parameter");
        let right = function
            .append_block_parameter(entry, types.bytes)
            .expect("right Bytes parameter");
        let index = function
            .append_block_parameter(entry, types.integer)
            .expect("index parameter");
        function
            .append_instruction(
                entry,
                InstructionKind::TextEncodeUtf8 { text },
                &[types.bytes],
                origin(),
            )
            .expect("encode");
        function
            .append_instruction(
                entry,
                InstructionKind::BytesLength { bytes: left },
                &[types.integer],
                origin(),
            )
            .expect("length");
        function
            .append_instruction(
                entry,
                InstructionKind::BytesGet {
                    bytes: left,
                    index,
                    missing_variant: 0,
                    found_variant: 1,
                },
                &[types.option_int],
                origin(),
            )
            .expect("get");
        let append = function
            .append_instruction(
                entry,
                InstructionKind::BytesAppend { left, right },
                &[types.bytes],
                origin(),
            )
            .expect("append");
        let decode = function
            .append_instruction(
                entry,
                InstructionKind::BytesDecodeUtf8 {
                    bytes: append[0],
                    ok_variant: 0,
                    error_variant: 1,
                    invalid_utf8_variant: 0,
                },
                &[types.decode_result],
                origin(),
            )
            .expect("decode");
        function
            .append_instruction(
                entry,
                InstructionKind::BytesCompare {
                    predicate: BoolPredicate::NotEqual,
                    left,
                    right,
                },
                &[types.boolean],
                origin(),
            )
            .expect("compare");
        let zero = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(0)),
                &[types.integer],
                origin(),
            )
            .expect("zero")[0];
        let lower_proof = function
            .append_instruction(
                entry,
                InstructionKind::IntCompare {
                    predicate: IntPredicate::GreaterEqual,
                    left: index,
                    right: zero,
                },
                &[types.boolean],
                origin(),
            )
            .expect("lower proof")[0];
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Branch {
                        condition: lower_proof,
                        then_target: BlockTarget::new(lower_checked, []),
                        else_target: BlockTarget::new(lower_failed, []),
                    },
                    origin(),
                ),
            )
            .expect("lower guard");
        let lower_failure = function
            .append_instruction(
                lower_failed,
                InstructionKind::Constant(Constant::Unit),
                &[types.unit],
                origin(),
            )
            .expect("lower failure")[0];
        function
            .terminate(
                lower_failed,
                Terminator::new(TerminatorKind::Return(lower_failure), origin()),
            )
            .expect("lower failure return");
        let maximum = function
            .append_instruction(
                lower_checked,
                InstructionKind::Constant(Constant::Int(255)),
                &[types.integer],
                origin(),
            )
            .expect("maximum")[0];
        let upper_proof = function
            .append_instruction(
                lower_checked,
                InstructionKind::IntCompare {
                    predicate: IntPredicate::LessEqual,
                    left: index,
                    right: maximum,
                },
                &[types.boolean],
                origin(),
            )
            .expect("upper proof")[0];
        function
            .terminate(
                lower_checked,
                Terminator::new(
                    TerminatorKind::Branch {
                        condition: upper_proof,
                        then_target: BlockTarget::new(push_block, []),
                        else_target: BlockTarget::new(upper_failed, []),
                    },
                    origin(),
                ),
            )
            .expect("upper guard");
        let upper_failure = function
            .append_instruction(
                upper_failed,
                InstructionKind::Constant(Constant::Unit),
                &[types.unit],
                origin(),
            )
            .expect("upper failure")[0];
        function
            .terminate(
                upper_failed,
                Terminator::new(TerminatorKind::Return(upper_failure), origin()),
            )
            .expect("upper failure return");
        let push = function
            .append_instruction(
                push_block,
                InstructionKind::BytesPush {
                    bytes: left,
                    unit: index,
                    lower_proof,
                    upper_proof,
                },
                &[types.bytes],
                origin(),
            )
            .expect("push");
        let result = function
            .append_instruction(
                push_block,
                InstructionKind::Constant(Constant::Unit),
                &[types.unit],
                origin(),
            )
            .expect("Unit")[0];
        function
            .terminate(
                push_block,
                Terminator::new(TerminatorKind::Return(result), origin()),
            )
            .expect("return");
        (append[0], push[0], decode[0], left)
    };
    let program = builder.finish_checked().expect("checked Bytes program");
    let function = program.as_program().function(root).expect("function");
    let instruction_id = |value| match function.value(value).expect("value").definition() {
        loom_codegen_ir::ValueDefinition::InstructionResult { instruction, .. } => instruction,
        loom_codegen_ir::ValueDefinition::BlockParameter { .. } => {
            panic!("expected instruction result")
        }
    };
    let roots = plan_managed_roots(&program, root).expect("managed roots");
    assert!(
        roots
            .state(ManagedSafepoint::Instruction(instruction_id(append_id)))
            .is_some()
    );
    let push_state = roots
        .state(ManagedSafepoint::Instruction(instruction_id(push_id)))
        .expect("Bytes push root state");
    let left_slot = roots
        .slots()
        .iter()
        .position(|slot| slot.value() == left_id && slot.projection().is_empty())
        .expect("dead-after-push Bytes receiver root");
    let push_bitmap = roots.bitmaps()
        [usize::try_from(push_state).expect("root state") * roots.bitmap_words() + left_slot / 64];
    assert_ne!(push_bitmap & (1_u64 << (left_slot % 64)), 0);
    assert!(
        roots
            .state(ManagedSafepoint::Instruction(instruction_id(decode_id)))
            .is_none(),
        "Bytes.decode_utf8 relabels validated storage without collecting"
    );
    let dump = dump_program(&program);
    for opcode in [
        "text.encode_utf8",
        "bytes.length",
        "bytes.get",
        "bytes.append",
        "bytes.push",
        "bytes.decode_utf8",
        "bytes.compare.not_equal",
    ] {
        assert!(dump.contains(opcode), "missing {opcode}: {dump}");
    }
}

#[test]
fn raw_builder_cannot_forge_unique_bytes_push() {
    let (mut builder, types) = builder_with_bytes_types();
    let root = builder
        .declare_function(
            origin(),
            "bytes.unique.raw",
            Signature::new([], types.bytes),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    let mut function = builder.function(root).expect("function builder");
    let entry = function.create_block().expect("entry");
    function.set_entry(entry).expect("set entry");
    let text = function
        .append_instruction(
            entry,
            InstructionKind::TextLiteral { utf8: "x".into() },
            &[types.text],
            origin(),
        )
        .expect("Text")[0];
    let bytes = function
        .append_instruction(
            entry,
            InstructionKind::TextEncodeUtf8 { text },
            &[types.bytes],
            origin(),
        )
        .expect("Bytes")[0];
    let unit = function
        .append_instruction(
            entry,
            InstructionKind::Constant(Constant::Int(33)),
            &[types.integer],
            origin(),
        )
        .expect("unit")[0];
    let error = function
        .append_instruction(
            entry,
            InstructionKind::BytesPushUnique {
                bytes,
                unit,
                lower_proof: unit,
                upper_proof: unit,
            },
            &[types.bytes],
            origin(),
        )
        .expect_err("raw unique certificate must be rejected");
    assert_eq!(error.code(), BuildErrorCode::TrustedInstruction);
}

#[test]
fn checked_bytes_push_rejects_exact_comparisons_without_success_guards() {
    let (mut builder, types) = builder_with_bytes_types();
    let root = builder
        .declare_function(
            origin(),
            "bytes.push.missing_guards",
            Signature::new([types.bytes, types.integer], types.bytes),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let bytes = function
            .append_block_parameter(entry, types.bytes)
            .expect("Bytes parameter");
        let unit = function
            .append_block_parameter(entry, types.integer)
            .expect("unit parameter");
        let zero = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(0)),
                &[types.integer],
                origin(),
            )
            .expect("zero")[0];
        let maximum = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(255)),
                &[types.integer],
                origin(),
            )
            .expect("maximum")[0];
        let lower_proof = function
            .append_instruction(
                entry,
                InstructionKind::IntCompare {
                    predicate: IntPredicate::GreaterEqual,
                    left: unit,
                    right: zero,
                },
                &[types.boolean],
                origin(),
            )
            .expect("lower proof")[0];
        let upper_proof = function
            .append_instruction(
                entry,
                InstructionKind::IntCompare {
                    predicate: IntPredicate::LessEqual,
                    left: unit,
                    right: maximum,
                },
                &[types.boolean],
                origin(),
            )
            .expect("upper proof")[0];
        let pushed = function
            .append_instruction(
                entry,
                InstructionKind::BytesPush {
                    bytes,
                    unit,
                    lower_proof,
                    upper_proof,
                },
                &[types.bytes],
                origin(),
            )
            .expect("push")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(pushed), origin()),
            )
            .expect("return");
    }

    let errors = builder
        .finish_checked()
        .expect_err("unguarded byte-range comparisons must fail closed");
    for proof in ["lower_proof", "upper_proof"] {
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code() == ValidationCode::InvalidIntegerProof
                    && error.path().ends_with(proof)
                    && error.message().contains("must condition a reachable guard")
            }),
            "missing {proof} rejection: {errors:?}"
        );
    }
}

#[test]
fn checked_bytes_push_rejects_proofs_for_the_wrong_range() {
    let (mut builder, types) = builder_with_bytes_types();
    let root = builder
        .declare_function(
            origin(),
            "bytes.push.wrong_range",
            Signature::new([types.bytes, types.integer], types.bytes),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let bytes = function
            .append_block_parameter(entry, types.bytes)
            .expect("Bytes parameter");
        let unit = function
            .append_block_parameter(entry, types.integer)
            .expect("unit parameter");
        let one = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(1)),
                &[types.integer],
                origin(),
            )
            .expect("one")[0];
        let beyond_byte = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(256)),
                &[types.integer],
                origin(),
            )
            .expect("beyond byte")[0];
        let lower_proof = function
            .append_instruction(
                entry,
                InstructionKind::IntCompare {
                    predicate: IntPredicate::GreaterEqual,
                    left: unit,
                    right: one,
                },
                &[types.boolean],
                origin(),
            )
            .expect("wrong lower proof")[0];
        let upper_proof = function
            .append_instruction(
                entry,
                InstructionKind::IntCompare {
                    predicate: IntPredicate::LessEqual,
                    left: unit,
                    right: beyond_byte,
                },
                &[types.boolean],
                origin(),
            )
            .expect("wrong upper proof")[0];
        let pushed = function
            .append_instruction(
                entry,
                InstructionKind::BytesPush {
                    bytes,
                    unit,
                    lower_proof,
                    upper_proof,
                },
                &[types.bytes],
                origin(),
            )
            .expect("push")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(pushed), origin()),
            )
            .expect("return");
    }

    let errors = builder
        .finish_checked()
        .expect_err("wrong byte-range comparisons must fail closed");
    for (proof, relation) in [("lower_proof", "unit >= 0"), ("upper_proof", "unit <= 255")] {
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code() == ValidationCode::InvalidIntegerProof
                    && error.path().ends_with(proof)
                    && error.message().contains(relation)
            }),
            "missing {proof} exact-range rejection: {errors:?}"
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the hostile diamond must expose the false path around one otherwise exact guard"
)]
fn checked_bytes_push_rejects_a_success_edge_that_does_not_dominate_the_push() {
    let (mut builder, types) = builder_with_bytes_types();
    let root = builder
        .declare_function(
            origin(),
            "bytes.push.non_dominating_guard",
            Signature::new([types.bytes, types.integer], types.bytes),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        let upper_checked = function.create_block().expect("upper checked");
        let upper_failed = function.create_block().expect("upper failed");
        let lower_checked = function.create_block().expect("lower checked");
        let lower_bypass = function.create_block().expect("lower bypass");
        let push_block = function.create_block().expect("push block");
        function.set_entry(entry).expect("set entry");
        let bytes = function
            .append_block_parameter(entry, types.bytes)
            .expect("Bytes parameter");
        let unit = function
            .append_block_parameter(entry, types.integer)
            .expect("unit parameter");
        let zero = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(0)),
                &[types.integer],
                origin(),
            )
            .expect("zero")[0];
        let maximum = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(255)),
                &[types.integer],
                origin(),
            )
            .expect("maximum")[0];
        let lower_proof = function
            .append_instruction(
                entry,
                InstructionKind::IntCompare {
                    predicate: IntPredicate::GreaterEqual,
                    left: unit,
                    right: zero,
                },
                &[types.boolean],
                origin(),
            )
            .expect("lower proof")[0];
        let upper_proof = function
            .append_instruction(
                entry,
                InstructionKind::IntCompare {
                    predicate: IntPredicate::LessEqual,
                    left: unit,
                    right: maximum,
                },
                &[types.boolean],
                origin(),
            )
            .expect("upper proof")[0];
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Branch {
                        condition: upper_proof,
                        then_target: BlockTarget::new(upper_checked, []),
                        else_target: BlockTarget::new(upper_failed, []),
                    },
                    origin(),
                ),
            )
            .expect("upper guard");
        function
            .terminate(
                upper_failed,
                Terminator::new(TerminatorKind::Return(bytes), origin()),
            )
            .expect("upper failure return");
        function
            .terminate(
                upper_checked,
                Terminator::new(
                    TerminatorKind::Branch {
                        condition: lower_proof,
                        then_target: BlockTarget::new(lower_checked, []),
                        else_target: BlockTarget::new(lower_bypass, []),
                    },
                    origin(),
                ),
            )
            .expect("lower guard");
        for (block, label) in [
            (lower_checked, "lower success"),
            (lower_bypass, "lower bypass"),
        ] {
            function
                .terminate(
                    block,
                    Terminator::new(
                        TerminatorKind::Jump(BlockTarget::new(push_block, [])),
                        origin(),
                    ),
                )
                .unwrap_or_else(|error| panic!("{label} jump: {error:?}"));
        }
        let pushed = function
            .append_instruction(
                push_block,
                InstructionKind::BytesPush {
                    bytes,
                    unit,
                    lower_proof,
                    upper_proof,
                },
                &[types.bytes],
                origin(),
            )
            .expect("push")[0];
        function
            .terminate(
                push_block,
                Terminator::new(TerminatorKind::Return(pushed), origin()),
            )
            .expect("return");
    }

    let errors = builder
        .finish_checked()
        .expect_err("a bypassable success edge must fail closed");
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InvalidIntegerProof
                && error.path().ends_with("lower_proof")
                && error.message().contains("does not dominate the push")
        }),
        "missing non-dominating guard rejection: {errors:?}"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one hostile function rechecks operand, variant, and effect failures together"
)]
fn independent_validation_rejects_wrong_bytes_operands_variants_and_effects() {
    let (mut builder, types) = builder_with_bytes_types();
    let root = builder
        .declare_function(
            origin(),
            "invalid_bytes",
            Signature::new([types.bytes, types.integer], types.unit),
            Effects::NONE,
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let bytes = function
            .append_block_parameter(entry, types.bytes)
            .expect("Bytes parameter");
        let integer = function
            .append_block_parameter(entry, types.integer)
            .expect("Int parameter");
        function
            .append_instruction(
                entry,
                InstructionKind::BytesLength { bytes: integer },
                &[types.integer],
                origin(),
            )
            .expect("malformed length");
        function
            .append_instruction(
                entry,
                InstructionKind::BytesGet {
                    bytes,
                    index: integer,
                    missing_variant: 1,
                    found_variant: 0,
                },
                &[types.option_int],
                origin(),
            )
            .expect("malformed get");
        function
            .append_instruction(
                entry,
                InstructionKind::BytesAppend {
                    left: bytes,
                    right: bytes,
                },
                &[types.bytes],
                origin(),
            )
            .expect("collecting append");
        function
            .append_instruction(
                entry,
                InstructionKind::BytesPush {
                    bytes: integer,
                    unit: bytes,
                    lower_proof: bytes,
                    upper_proof: bytes,
                },
                &[types.bytes],
                origin(),
            )
            .expect("malformed collecting push");
        function
            .append_instruction(
                entry,
                InstructionKind::BytesDecodeUtf8 {
                    bytes,
                    ok_variant: 1,
                    error_variant: 0,
                    invalid_utf8_variant: 1,
                },
                &[types.decode_result],
                origin(),
            )
            .expect("malformed decode");
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
        .expect_err("malformed Bytes program must fail closed");
    assert!(
        errors
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::TypeMismatch),
        "{errors:?}"
    );
    assert!(
        errors
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::InstructionShape),
        "{errors:?}"
    );
    assert!(
        errors
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::EffectMismatch),
        "{errors:?}"
    );
}

#[test]
fn bytes_decode_rejects_a_transparent_decode_error_carrier() {
    let mut builder = ProgramBuilder::with_canonical_types(
        TargetLayout::new(64).expect("target"),
        bytes_catalog(),
    );
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    builder
        .add_managed_text_type()
        .expect("canonical managed Text");
    let bytes = builder
        .add_managed_bytes_type(Type::Nominal(BYTES_TYPE_ID, Vec::new()))
        .expect("canonical managed Bytes");
    let dummy_error = Type::Nominal(TypeId(90), Vec::new());
    builder
        .add_sum_type(dummy_error.clone(), &[Box::new([])])
        .expect("dummy closed error");
    let decode_error = Type::Nominal(DECODE_TEXT_ERROR_TYPE_ID, Vec::new());
    builder
        .add_transparent_type(decode_error.clone(), &dummy_error)
        .expect("layout-compatible but noncanonical DecodeTextError");
    let result = builder
        .add_sum_type(
            Type::Nominal(RESULT_TYPE_ID, vec![Type::Text, decode_error.clone()]),
            &[Box::from([Type::Text]), Box::from([decode_error])],
        )
        .expect("Result[Text, DecodeTextError]");
    let root = builder
        .declare_function(
            origin(),
            "transparent_decode_error",
            Signature::new([bytes], unit),
            Effects::NONE,
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let bytes = function
            .append_block_parameter(entry, bytes)
            .expect("Bytes parameter");
        function
            .append_instruction(
                entry,
                InstructionKind::BytesDecodeUtf8 {
                    bytes,
                    ok_variant: 0,
                    error_variant: 1,
                    invalid_utf8_variant: 0,
                },
                &[result],
                origin(),
            )
            .expect("Bytes.decode_utf8");
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
        .expect_err("DecodeTextError must not reuse a transparent representation boundary");
    assert!(
        errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstructionShape
                && error.message().contains("canonical direct DecodeTextError")
        }),
        "{errors:#?}"
    );
}

#[test]
fn bytes_opcodes_fail_closed_without_the_catalog_identity() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let root = builder
        .declare_function(
            origin(),
            "missing_bytes",
            Signature::new([integer], unit),
            Effects::NONE,
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let forged = function
            .append_block_parameter(entry, integer)
            .expect("forged Bytes");
        function
            .append_instruction(
                entry,
                InstructionKind::BytesLength { bytes: forged },
                &[integer],
                origin(),
            )
            .expect("forged Bytes length");
        let result = function
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
                Terminator::new(TerminatorKind::Return(result), origin()),
            )
            .expect("return");
    }
    let errors = builder
        .finish_checked()
        .expect_err("missing canonical Bytes identity must fail closed");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error.message().contains("canonical managed Bytes")
    }));
}
