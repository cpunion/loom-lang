use loom_codegen_ir::{
    ArtifactRootRequest, ArtifactValidationCode, BlockTarget, BoolPredicate, Constant, Effects,
    InstructionKind, ManagedSafepoint, Origin, ProgramBuilder, Repr, Signature,
    TEXT_LITERAL_MAX_BYTES, TargetLayout, Terminator, TerminatorKind, ValidationCode,
    ValueDefinition, dump_program, plan_managed_roots,
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
#[expect(
    clippy::too_many_lines,
    reason = "one closed manual CFG makes dead-edge and exact live-after assertions reviewable together"
)]
fn managed_concat_has_exact_live_after_roots_and_ignores_dead_edge_arguments() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let text = builder
        .add_managed_text_type()
        .expect("register managed Text");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let root = builder
        .declare_function(
            origin(9),
            "main",
            Signature::new([], unit),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("root declaration");
    let (live, dead, unused, concat_result) = {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        let join = function.create_block().expect("join");
        function.set_entry(entry).expect("set entry");
        let live = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "live".into(),
                },
                &[text],
                origin(9),
            )
            .expect("live literal")[0];
        let dead = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "dead".into(),
                },
                &[text],
                origin(9),
            )
            .expect("dead literal")[0];
        let unused = function
            .append_block_parameter(join, text)
            .expect("unused edge parameter");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Jump(loom_codegen_ir::BlockTarget::new(join, [dead])),
                    origin(9),
                ),
            )
            .expect("jump");
        let left = function
            .append_instruction(
                join,
                InstructionKind::TextLiteral { utf8: "a".into() },
                &[text],
                origin(9),
            )
            .expect("left literal")[0];
        let right = function
            .append_instruction(
                join,
                InstructionKind::TextLiteral { utf8: "b".into() },
                &[text],
                origin(9),
            )
            .expect("right literal")[0];
        let concat_result = function
            .append_instruction(
                join,
                InstructionKind::TextConcat { left, right },
                &[text],
                origin(9),
            )
            .expect("concat")[0];
        function
            .append_instruction(
                join,
                InstructionKind::TextLength { text: live },
                &[integer],
                origin(9),
            )
            .expect("use live value after concat");
        let result = function
            .append_instruction(
                join,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(9),
            )
            .expect("Unit")[0];
        function
            .terminate(
                join,
                Terminator::new(TerminatorKind::Return(result), origin(9)),
            )
            .expect("return");
        (live, dead, unused, concat_result)
    };
    let artifact = builder
        .finish_checked()
        .expect("checked managed Text LCIR")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("closed managed Text artifact");
    let plan = plan_managed_roots(artifact.program(), root).expect("root plan");
    assert_eq!(plan.slots().len(), 1);
    assert_eq!(plan.slots()[0].value(), live);
    assert!(plan.slots()[0].projection().is_empty());
    assert!(!plan.slots().iter().any(|slot| slot.value() == dead));
    assert!(!plan.slots().iter().any(|slot| slot.value() == unused));
    assert!(
        !plan
            .slots()
            .iter()
            .any(|slot| slot.value() == concat_result)
    );
    assert_eq!(plan.bitmap_words(), 1);
    assert_eq!(plan.bitmaps(), [0, 1]);
    let ValueDefinition::InstructionResult { instruction, .. } = artifact
        .function(root)
        .and_then(|function| function.value(concat_result))
        .expect("concat result")
        .definition()
    else {
        panic!("concat result must be an instruction result")
    };
    assert_eq!(
        plan.state(ManagedSafepoint::Instruction(instruction)),
        Some(1)
    );
    assert_eq!(
        artifact
            .representations()
            .repr(artifact.representations().value_type(text).unwrap().repr()),
        Some(&Repr::ManagedPointer)
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one manual CFG keeps nested projections, aliases, phis, dead edge arguments, and pointer-free values auditable together"
)]
fn nested_managed_products_expand_only_live_ssa_values_to_stable_leaf_slots() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let text = builder
        .add_managed_text_type()
        .expect("register managed Text");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let inner_semantic = Type::Tuple(vec![Type::Text, Type::Int]);
    let inner = builder
        .add_tuple_type(&[Type::Text, Type::Int])
        .expect("inner managed product");
    let outer = builder
        .add_tuple_type(&[inner_semantic, Type::Text, Type::Bool])
        .expect("outer managed product");
    let pod = builder
        .add_tuple_type(&[Type::Int, Type::Bool])
        .expect("pointer-free product");
    let root = builder
        .declare_function(
            origin(10),
            "managed_products",
            Signature::new([], unit),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("root declaration");
    let (alias, dead, selected, unused, stable_pod, concat_result) = {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        let then_block = function.create_block().expect("then");
        let else_block = function.create_block().expect("else");
        let join = function.create_block().expect("join");
        function.set_entry(entry).expect("set entry");

        let alias = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "alias".into(),
                },
                &[text],
                origin(10),
            )
            .expect("alias")[0];
        let dead = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "dead".into(),
                },
                &[text],
                origin(10),
            )
            .expect("dead")[0];
        let number = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(7)),
                &[integer],
                origin(10),
            )
            .expect("number")[0];
        let condition = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[boolean],
                origin(10),
            )
            .expect("condition")[0];
        let inner_value = function
            .append_instruction(
                entry,
                InstructionKind::ProductConstruct {
                    fields: Box::from([alias, number]),
                },
                &[inner],
                origin(10),
            )
            .expect("inner")[0];
        let outer_value = function
            .append_instruction(
                entry,
                InstructionKind::ProductConstruct {
                    fields: Box::from([inner_value, alias, condition]),
                },
                &[outer],
                origin(10),
            )
            .expect("outer")[0];
        let dead_inner = function
            .append_instruction(
                entry,
                InstructionKind::ProductConstruct {
                    fields: Box::from([dead, number]),
                },
                &[inner],
                origin(10),
            )
            .expect("dead inner")[0];
        let dead_outer = function
            .append_instruction(
                entry,
                InstructionKind::ProductConstruct {
                    fields: Box::from([dead_inner, dead, condition]),
                },
                &[outer],
                origin(10),
            )
            .expect("dead outer")[0];
        let pod_value = function
            .append_instruction(
                entry,
                InstructionKind::ProductConstruct {
                    fields: Box::from([number, condition]),
                },
                &[pod],
                origin(10),
            )
            .expect("pointer-free product")[0];
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(then_block, []),
                        else_target: BlockTarget::new(else_block, []),
                    },
                    origin(10),
                ),
            )
            .expect("branch");

        let selected = function
            .append_block_parameter(join, outer)
            .expect("selected phi");
        let unused = function
            .append_block_parameter(join, outer)
            .expect("unused phi");
        let stable_pod = function.append_block_parameter(join, pod).expect("POD phi");
        for block in [then_block, else_block] {
            function
                .terminate(
                    block,
                    Terminator::new(
                        TerminatorKind::Jump(BlockTarget::new(
                            join,
                            [outer_value, dead_outer, pod_value],
                        )),
                        origin(10),
                    ),
                )
                .expect("join edge");
        }

        let safepoint = function
            .append_instruction(
                join,
                InstructionKind::TextConcat {
                    left: alias,
                    right: alias,
                },
                &[text],
                origin(10),
            )
            .expect("concat");
        let concat_result = safepoint[0];
        let selected_inner = function
            .append_instruction(
                join,
                InstructionKind::ProductExtract {
                    aggregate: selected,
                    field: 0,
                },
                &[inner],
                origin(10),
            )
            .expect("extract inner")[0];
        let kept = function
            .append_instruction(
                join,
                InstructionKind::ProductExtract {
                    aggregate: selected_inner,
                    field: 0,
                },
                &[text],
                origin(10),
            )
            .expect("extract managed leaf")[0];
        function
            .append_instruction(
                join,
                InstructionKind::TextLength { text: kept },
                &[integer],
                origin(10),
            )
            .expect("use managed leaf");
        function
            .append_instruction(
                join,
                InstructionKind::ProductExtract {
                    aggregate: stable_pod,
                    field: 0,
                },
                &[integer],
                origin(10),
            )
            .expect("use pointer-free product");
        let result = function
            .append_instruction(
                join,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(10),
            )
            .expect("Unit")[0];
        function
            .terminate(
                join,
                Terminator::new(TerminatorKind::Return(result), origin(10)),
            )
            .expect("return");
        (alias, dead, selected, unused, stable_pod, concat_result)
    };
    let artifact = builder
        .finish_checked()
        .expect("checked managed-product LCIR")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("closed managed-product artifact");
    let plan = plan_managed_roots(artifact.program(), root).expect("root plan");
    assert_eq!(plan.slots().len(), 2);
    assert!(plan.slots().iter().all(|slot| slot.value() == selected));
    assert_eq!(plan.slots()[0].projection(), [0, 0]);
    assert_eq!(plan.slots()[1].projection(), [1]);
    for excluded in [alias, dead, unused, stable_pod, concat_result] {
        assert!(!plan.slots().iter().any(|slot| slot.value() == excluded));
    }
    assert_eq!(plan.bitmap_words(), 1);
    assert_eq!(plan.bitmaps(), [0, 3]);
    let ValueDefinition::InstructionResult { instruction, .. } = artifact
        .function(root)
        .and_then(|function| function.value(concat_result))
        .expect("concat result")
        .definition()
    else {
        panic!("concat result must have an instruction definition")
    };
    assert_eq!(
        plan.state(ManagedSafepoint::Instruction(instruction)),
        Some(1)
    );
}

#[test]
fn independent_validation_requires_a_canonical_text_pointer_representation() {
    let mut missing = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let integer = missing.type_id(&Type::Int).expect("Int");
    let boolean = missing.type_id(&Type::Bool).expect("Bool");
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
            .append_instruction(
                entry,
                InstructionKind::TextLength { text: forged },
                &[integer],
                origin(2),
            )
            .expect("unchecked Text length");
        function
            .append_instruction(
                entry,
                InstructionKind::TextContains {
                    text: forged,
                    needle: forged,
                },
                &[boolean],
                origin(2),
            )
            .expect("unchecked Text containment");
        function
            .append_instruction(
                entry,
                InstructionKind::TextCompare {
                    predicate: BoolPredicate::Equal,
                    left: forged,
                    right: forged,
                },
                &[boolean],
                origin(2),
            )
            .expect("unchecked Text comparison");
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
    for message in [
        "Text literal requires the canonical Text pointer representation",
        "Text length requires the canonical Text pointer representation",
        "Text containment requires the canonical Text pointer representation",
        "Text comparison requires the canonical Text pointer representation",
    ] {
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::TypeMismatch && error.message() == message
        }));
    }
}

#[test]
fn independent_validation_rejects_oversized_text_literals() {
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
