use loom_codegen_ir::{
    ArtifactRootRequest, ArtifactValidationCode, BoolPredicate, Constant, Effects, InstructionKind,
    Origin, ProgramBuilder, Repr, Signature, TEXT_LITERAL_MAX_BYTES, TargetLayout, Terminator,
    TerminatorKind, ValidationCode, dump_program,
};
use loom_mir::{FunctionId as MirFunctionId, Type};

fn origin(function: u32) -> Origin {
    Origin::synthetic(MirFunctionId(function))
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one closed manual program exercises every immortal Text edge in a single artifact"
)]
fn checked_immortal_text_closure_has_only_typed_construction_and_operations() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let text = builder
        .add_immortal_text_type()
        .expect("register immortal Text");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let inspect = builder
        .declare_function(
            origin(0),
            "inspect",
            Signature::new([text, text], boolean),
            Effects::NONE,
        )
        .expect("inspect declaration");
    let root = builder
        .declare_function(origin(1), "main", Signature::new([], unit), Effects::NONE)
        .expect("root declaration");
    {
        let mut function = builder.function(inspect).expect("inspect builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let left = function
            .append_block_parameter(entry, text)
            .expect("left parameter");
        let right = function
            .append_block_parameter(entry, text)
            .expect("right parameter");
        let equal = function
            .append_instruction(
                entry,
                InstructionKind::TextCompare {
                    predicate: BoolPredicate::Equal,
                    left,
                    right,
                },
                &[boolean],
                origin(0),
            )
            .expect("content comparison")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(equal), origin(0)),
            )
            .expect("return");
    }
    {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let first = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "hello界".into(),
                },
                &[text],
                origin(1),
            )
            .expect("first literal")[0];
        let second = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "hello界".into(),
                },
                &[text],
                origin(1),
            )
            .expect("second literal")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::TextLength { text: first },
                &[integer],
                origin(1),
            )
            .expect("length");
        function
            .append_instruction(
                entry,
                InstructionKind::TextContains {
                    text: first,
                    needle: second,
                },
                &[boolean],
                origin(1),
            )
            .expect("contains");
        function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: inspect,
                    arguments: Box::from([first, second]),
                },
                &[boolean],
                origin(1),
            )
            .expect("inspect call");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(1),
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin(1)),
            )
            .expect("return");
    }
    let artifact = builder
        .finish_checked()
        .expect("checked Text LCIR")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("closed Text artifact");
    let dump = dump_program(artifact.program());
    assert!(dump.contains("immortal_text_ptr"), "{dump}");
    assert!(dump.contains("text.compare.equal"), "{dump}");
    assert_eq!(
        artifact
            .representations()
            .repr(artifact.representations().value_type(text).unwrap().repr()),
        Some(&Repr::ImmortalText)
    );
}

#[test]
fn independent_validation_rejects_forged_or_oversized_text_literals() {
    let mut missing = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let integer = missing.type_id(&Type::Int).expect("Int");
    let function = missing
        .declare_function(
            origin(2),
            "forged",
            Signature::new([], integer),
            Effects::NONE,
        )
        .expect("declaration");
    {
        let mut function = missing.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let forged = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "forged".into(),
                },
                &[integer],
                origin(2),
            )
            .expect("unchecked instruction")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(forged), origin(2)),
            )
            .expect("return");
    }
    let errors = missing
        .finish_checked()
        .expect_err("Text literal without its representation must fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error.message().contains("canonical immortal Text")
    }));

    let mut oversized = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let text = oversized.add_immortal_text_type().expect("register Text");
    let function = oversized
        .declare_function(
            origin(3),
            "oversized",
            Signature::new([], text),
            Effects::NONE,
        )
        .expect("declaration");
    {
        let mut function = oversized.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let literal = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "x".repeat(TEXT_LITERAL_MAX_BYTES + 1).into_boxed_str(),
                },
                &[text],
                origin(3),
            )
            .expect("unchecked oversized literal")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(literal), origin(3)),
            )
            .expect("return");
    }
    let errors = oversized
        .finish_checked()
        .expect_err("oversized literal must fail validation");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape
            && error.message().contains("per-literal budget")
    }));
}

#[test]
fn artifact_roots_cannot_import_an_immortal_text_pointer() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let text = builder.add_immortal_text_type().expect("register Text");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let root = builder
        .declare_function(
            origin(4),
            "external_text_root",
            Signature::new([text], unit),
            Effects::NONE,
        )
        .expect("root declaration");
    {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        function
            .append_block_parameter(entry, text)
            .expect("external Text parameter");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(4),
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin(4)),
            )
            .expect("return");
    }
    let errors = builder
        .finish_checked()
        .expect("structurally valid parameterized Text function")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect_err("a root must not import a supposedly immortal pointer");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ArtifactValidationCode::RootSignature
            && error.message().contains("zero parameters")
    }));
}
