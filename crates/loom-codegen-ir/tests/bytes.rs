use loom_codegen_ir::{
    BYTES_TYPE_ID, BoolPredicate, Constant, Effects, InstructionKind, ManagedSafepoint, Origin,
    ProgramBuilder, Signature, TargetLayout, Terminator, TerminatorKind, ValidationCode,
    ValueTypeId, dump_program, plan_managed_roots,
};
use loom_mir::{FunctionId as MirFunctionId, Type, TypeId};

const OPTION_TYPE_ID: TypeId = TypeId(0);
const RESULT_TYPE_ID: TypeId = TypeId(1);
const DECODE_TEXT_ERROR_TYPE_ID: TypeId = TypeId(13);

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
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
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
    let (append_id, decode_id) = {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
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
        (append[0], decode[0])
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
    assert!(
        roots
            .state(ManagedSafepoint::Instruction(instruction_id(decode_id)))
            .is_some()
    );
    let dump = dump_program(&program);
    for opcode in [
        "text.encode_utf8",
        "bytes.length",
        "bytes.get",
        "bytes.append",
        "bytes.decode_utf8",
        "bytes.compare.not_equal",
    ] {
        assert!(dump.contains(opcode), "missing {opcode}: {dump}");
    }
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
fn bytes_opcodes_fail_closed_without_the_canonical_registration() {
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
        .expect_err("missing canonical Bytes must fail closed");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error.message().contains("canonical managed Bytes#11")
    }));
}
