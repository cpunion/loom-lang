use loom_codegen_ir::{
    ArtifactRootRequest, ArtifactValidationCode, BlockTarget, BoolPredicate, Constant, Effects,
    InstructionKind, ManagedRootProjection, ManagedSafepoint, Origin, ProgramBuilder, Repr,
    Signature, TEXT_LITERAL_MAX_BYTES, TargetLayout, Terminator, TerminatorKind, ValidationCode,
    ValueDefinition, dump_program, plan_managed_roots,
};
use loom_mir::{FunctionId as MirFunctionId, Type, TypeId};

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
    reason = "one manual graph keeps Text.get result shape, live-after roots, and dump identity reviewable together"
)]
fn managed_text_get_has_a_checked_option_shape_and_exact_live_after_roots() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let text = builder
        .add_managed_text_type()
        .expect("register managed Text");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let option = builder
        .add_sum_type(
            Type::Nominal(TypeId(124), vec![Type::Text]),
            &[Box::new([]), Box::from([Type::Text])],
        )
        .expect("Option[Text]");
    let root = builder
        .declare_function(
            origin(13),
            "managed.text.get",
            Signature::new([], unit),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("root declaration");
    let (source, live, selected) = {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let source = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "a界".into()
                },
                &[text],
                origin(13),
            )
            .expect("source")[0];
        let live = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "live".into(),
                },
                &[text],
                origin(13),
            )
            .expect("live")[0];
        let index = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(1)),
                &[integer],
                origin(13),
            )
            .expect("index")[0];
        let selected = function
            .append_instruction(
                entry,
                InstructionKind::TextGet {
                    text: source,
                    index,
                    missing_variant: 0,
                    found_variant: 1,
                },
                &[option],
                origin(13),
            )
            .expect("Text.get")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::TextLength { text: live },
                &[integer],
                origin(13),
            )
            .expect("post-safepoint use");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(13),
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin(13)),
            )
            .expect("return");
        (source, live, selected)
    };
    let program = builder.finish_checked().expect("checked Text.get LCIR");
    let plan = plan_managed_roots(&program, root).expect("Text.get root plan");
    assert_eq!(plan.slots().len(), 1);
    assert_eq!(plan.slots()[0].value(), live);
    assert!(plan.slots()[0].projection().is_empty());
    assert!(!plan.slots().iter().any(|slot| slot.value() == source));
    assert!(!plan.slots().iter().any(|slot| slot.value() == selected));
    let ValueDefinition::InstructionResult { instruction, .. } = program
        .as_program()
        .function(root)
        .and_then(|function| function.value(selected))
        .expect("Text.get result")
        .definition()
    else {
        panic!("Text.get result must have an instruction definition")
    };
    assert_eq!(
        plan.state(ManagedSafepoint::Instruction(instruction)),
        Some(1)
    );
    let dump = dump_program(&program);
    assert!(
        dump.contains("text.get") && dump.contains("missing 0, found 1"),
        "{dump}"
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
    assert_eq!(
        plan.slots()[0].projection(),
        [
            ManagedRootProjection::ProductField(0),
            ManagedRootProjection::ProductField(0),
        ]
    );
    assert_eq!(
        plan.slots()[1].projection(),
        [ManagedRootProjection::ProductField(1)]
    );
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
#[expect(
    clippy::too_many_lines,
    reason = "one manual graph makes candidate variant order, tagless nesting, dead definitions, and safepoint-result liveness directly reviewable"
)]
fn managed_sums_catalog_every_variant_candidate_but_only_for_live_values() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let text = builder
        .add_managed_text_type()
        .expect("register managed Text");
    let tagless_semantic = Type::Nominal(TypeId(120), Vec::new());
    let tagless = builder
        .add_sum_type(tagless_semantic.clone(), &[Box::from([Type::Text])])
        .expect("tagless managed sum");
    let option_semantic = Type::Nominal(TypeId(121), Vec::new());
    let option = builder
        .add_sum_type(
            option_semantic.clone(),
            &[Box::new([]), Box::from([Type::Text])],
        )
        .expect("optional managed sum");
    let nested = builder
        .add_sum_type(
            Type::Nominal(TypeId(122), Vec::new()),
            &[
                Box::from([tagless_semantic]),
                Box::from([option_semantic]),
                Box::from([Type::Text, Type::Text]),
            ],
        )
        .expect("nested managed sum");
    let plain = builder
        .add_sum_type(
            Type::Nominal(TypeId(123), Vec::new()),
            &[Box::from([Type::Int]), Box::new([])],
        )
        .expect("pointer-free sum");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let consume = builder
        .declare_function(
            origin(11),
            "consume.sums",
            Signature::new([tagless, nested, plain], unit),
            Effects::NONE,
        )
        .expect("consume declaration");
    let root = builder
        .declare_function(
            origin(12),
            "managed.sums",
            Signature::new([], unit),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("root declaration");
    {
        let mut function = builder.function(consume).expect("consume builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        for ty in [tagless, nested, plain] {
            function
                .append_block_parameter(entry, ty)
                .expect("consume parameter");
        }
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(11),
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin(11)),
            )
            .expect("return");
    }
    let (tagless_value, nested_value, plain_value, concat_result) = {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let kept = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "kept".into(),
                },
                &[text],
                origin(12),
            )
            .expect("managed literal")[0];
        let tagless_value = function
            .append_instruction(
                entry,
                InstructionKind::SumConstruct {
                    variant: 0,
                    payload: Box::from([kept]),
                },
                &[tagless],
                origin(12),
            )
            .expect("tagless value")[0];
        let option_value = function
            .append_instruction(
                entry,
                InstructionKind::SumConstruct {
                    variant: 1,
                    payload: Box::from([kept]),
                },
                &[option],
                origin(12),
            )
            .expect("option value")[0];
        let nested_value = function
            .append_instruction(
                entry,
                InstructionKind::SumConstruct {
                    variant: 1,
                    payload: Box::from([option_value]),
                },
                &[nested],
                origin(12),
            )
            .expect("nested value")[0];
        let number = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(1)),
                &[integer],
                origin(12),
            )
            .expect("integer")[0];
        let plain_value = function
            .append_instruction(
                entry,
                InstructionKind::SumConstruct {
                    variant: 0,
                    payload: Box::from([number]),
                },
                &[plain],
                origin(12),
            )
            .expect("plain sum")[0];
        let concat_result = function
            .append_instruction(
                entry,
                InstructionKind::TextConcat {
                    left: kept,
                    right: kept,
                },
                &[text],
                origin(12),
            )
            .expect("safepoint result")[0];
        let result = function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: consume,
                    arguments: Box::from([tagless_value, nested_value, plain_value]),
                },
                &[unit],
                origin(12),
            )
            .expect("post-safepoint use")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin(12)),
            )
            .expect("return");
        (tagless_value, nested_value, plain_value, concat_result)
    };
    let program = builder.finish_checked().expect("checked managed-sum graph");
    let root_plan = plan_managed_roots(&program, root).expect("managed-sum root plan");
    let expected = [
        (
            tagless_value,
            vec![ManagedRootProjection::SumVariantField {
                variant: 0,
                field: 0,
            }],
        ),
        (
            nested_value,
            vec![
                ManagedRootProjection::SumVariantField {
                    variant: 0,
                    field: 0,
                },
                ManagedRootProjection::SumVariantField {
                    variant: 0,
                    field: 0,
                },
            ],
        ),
        (
            nested_value,
            vec![
                ManagedRootProjection::SumVariantField {
                    variant: 1,
                    field: 0,
                },
                ManagedRootProjection::SumVariantField {
                    variant: 1,
                    field: 0,
                },
            ],
        ),
        (
            nested_value,
            vec![ManagedRootProjection::SumVariantField {
                variant: 2,
                field: 0,
            }],
        ),
        (
            nested_value,
            vec![ManagedRootProjection::SumVariantField {
                variant: 2,
                field: 1,
            }],
        ),
    ];
    assert_eq!(root_plan.slots().len(), expected.len());
    for (slot, (value, projection)) in root_plan.slots().iter().zip(expected) {
        assert_eq!(slot.value(), value);
        assert_eq!(slot.projection(), projection);
    }
    assert!(
        !root_plan
            .slots()
            .iter()
            .any(|slot| { slot.value() == plain_value || slot.value() == concat_result })
    );
    assert_eq!(root_plan.bitmaps(), [0, 0b1_1111]);
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
#[expect(
    clippy::too_many_lines,
    reason = "one hostile graph exercises independent Text.get operand, variant, and semantic-result checks together"
)]
fn independent_validation_rejects_forged_text_get_operands_variants_and_result() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let text = builder
        .add_managed_text_type()
        .expect("register managed Text");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let reversed = builder
        .add_sum_type(
            Type::Nominal(TypeId(125), vec![Type::Text]),
            &[Box::from([Type::Text]), Box::new([])],
        )
        .expect("reversed Option-shaped sum");
    let wrong_semantic = builder
        .add_sum_type(
            Type::Nominal(TypeId(126), vec![Type::Int]),
            &[Box::new([]), Box::from([Type::Text])],
        )
        .expect("non-Option Text sum");
    let root = builder
        .declare_function(
            origin(14),
            "forged.text.get",
            Signature::new([], unit),
            Effects::MAY_COLLECT.with_implications(),
        )
        .expect("root declaration");
    {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let source = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral {
                    utf8: "value".into(),
                },
                &[text],
                origin(14),
            )
            .expect("source")[0];
        let wrong_index = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[boolean],
                origin(14),
            )
            .expect("wrong index")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::TextGet {
                    text: source,
                    index: wrong_index,
                    missing_variant: 0,
                    found_variant: 0,
                },
                &[reversed],
                origin(14),
            )
            .expect("forged variant mapping");
        let index = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(0)),
                &[integer],
                origin(14),
            )
            .expect("index")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::TextGet {
                    text: source,
                    index,
                    missing_variant: 0,
                    found_variant: 1,
                },
                &[wrong_semantic],
                origin(14),
            )
            .expect("forged result semantic");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin(14),
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(result), origin(14)),
            )
            .expect("return");
    }
    let errors = builder
        .finish_checked()
        .expect_err("forged Text.get instructions must fail validation");
    for message in [
        "Text selection requires distinct missing and found variants",
        "Text selection missing variant must exist and carry no payload",
        "Text selection result must be a nominal Option[Text]",
    ] {
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.message() == message),
            "missing `{message}` in {errors:?}"
        );
    }
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error.path().split('.').next_back() == Some("index")
    }));
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
