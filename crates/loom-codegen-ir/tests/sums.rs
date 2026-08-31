use loom_codegen_ir::{
    ArtifactRootRequest, ArtifactValidationCode, Effects, InstructionKind, Origin, ProgramBuilder,
    Repr, Signature, SumCase, SumTagRepr, TargetLayout, Terminator, TerminatorKind,
    TestOutcomePlan, ValidationCode, artifact_identity, validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

fn nominal(raw: u32) -> Type {
    Type::Nominal(TypeId(raw), Vec::new())
}

#[test]
fn sum_plan_selects_tagless_tag_only_and_tagged_shapes() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let tagless = builder
        .add_sum_type(nominal(10), &[Box::from([Type::Int])])
        .expect("single variant sum");
    let tag_only = builder
        .add_sum_type(nominal(11), &[Box::new([]), Box::new([]), Box::new([])])
        .expect("empty variants");
    let tagged = builder
        .add_sum_type(
            nominal(12),
            &[Box::new([]), Box::from([Type::Int, Type::Bool])],
        )
        .expect("payload variants");

    let representations = builder.representations();
    let sum = |ty| {
        let value = representations.value_type(ty).expect("value type");
        let Repr::Sum(sum) = representations
            .repr(value.repr())
            .copied()
            .expect("representation")
        else {
            panic!("expected sum representation")
        };
        representations.sum(sum).expect("sum definition")
    };
    assert_eq!(sum(tagless).tag(), SumTagRepr::Tagless);
    assert!(!sum(tagless).is_tag_only());
    assert_eq!(sum(tag_only).tag(), SumTagRepr::I8);
    assert!(sum(tag_only).is_tag_only());
    assert_eq!(sum(tagged).tag(), SumTagRepr::I8);
    assert!(!sum(tagged).is_tag_only());
    assert_ne!(tagless, tag_only);
    assert_ne!(tag_only, tagged);
}

#[test]
fn exhaustive_sum_switch_carries_typed_payload_edge_parameters() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let sum = builder
        .add_sum_type(nominal(20), &[Box::from([Type::Int]), Box::new([])])
        .expect("sum");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let origin = Origin::synthetic(FunctionId(20));
    let root = builder
        .declare_function(
            origin,
            "sum.switch.valid",
            Signature::new(Vec::new(), unit),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = builder.function(root).expect("builder");
        let entry = function.create_block().expect("entry");
        let some = function.create_block().expect("payload case");
        let none = function.create_block().expect("empty case");
        function.set_entry(entry).expect("set entry");
        function
            .append_block_parameter(some, integer)
            .expect("implicit payload");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(loom_codegen_ir::Constant::Int(7)),
                &[integer],
                origin,
            )
            .expect("integer")[0];
        let sum_value = function
            .append_instruction(
                entry,
                InstructionKind::SumConstruct {
                    variant: 0,
                    payload: Box::from([value]),
                },
                &[sum],
                origin,
            )
            .expect("construct")[0];
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::SumSwitch {
                        scrutinee: sum_value,
                        cases: Box::from([SumCase::new(0, some, []), SumCase::new(1, none, [])]),
                    },
                    origin,
                ),
            )
            .expect("switch");
        for case in [some, none] {
            let result = function
                .append_instruction(
                    case,
                    InstructionKind::Constant(loom_codegen_ir::Constant::Unit),
                    &[unit],
                    origin,
                )
                .expect("Unit")[0];
            function
                .terminate(
                    case,
                    Terminator::new(TerminatorKind::Return(result), origin),
                )
                .expect("return");
        }
    }
    builder.finish_checked().expect("valid exhaustive sum CFG");
}

#[test]
fn sum_zip_switch_injects_both_payloads_in_left_then_right_order() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let sum = builder
        .add_sum_type(
            nominal(21),
            &[Box::from([Type::Int, Type::Bool]), Box::new([])],
        )
        .expect("sum");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let origin = Origin::synthetic(FunctionId(21));
    let root = builder
        .declare_function(
            origin,
            "sum.zip.valid",
            Signature::new([sum, sum, boolean], unit),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = builder.function(root).expect("builder");
        let entry = function.create_block().expect("entry");
        let payload = function.create_block().expect("payload case");
        let empty = function.create_block().expect("empty case");
        let mismatch = function.create_block().expect("mismatch");
        function.set_entry(entry).expect("set entry");
        let left = function
            .append_block_parameter(entry, sum)
            .expect("left sum");
        let right = function
            .append_block_parameter(entry, sum)
            .expect("right sum");
        let forwarded = function
            .append_block_parameter(entry, boolean)
            .expect("forwarded value");
        for ty in [integer, boolean, integer, boolean] {
            function
                .append_block_parameter(payload, ty)
                .expect("paired payload");
        }
        function
            .append_block_parameter(payload, boolean)
            .expect("payload forwarded value");
        function
            .append_block_parameter(empty, boolean)
            .expect("empty forwarded value");
        function
            .append_block_parameter(mismatch, boolean)
            .expect("mismatch forwarded value");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::SumZipSwitch {
                        left,
                        right,
                        cases: Box::from([
                            SumCase::new(0, payload, [forwarded]),
                            SumCase::new(1, empty, [forwarded]),
                        ]),
                        mismatch: loom_codegen_ir::BlockTarget::new(mismatch, [forwarded]),
                    },
                    origin,
                ),
            )
            .expect("zip switch");
        for block in [payload, empty, mismatch] {
            let result = function
                .append_instruction(
                    block,
                    InstructionKind::Constant(loom_codegen_ir::Constant::Unit),
                    &[unit],
                    origin,
                )
                .expect("Unit")[0];
            function
                .terminate(
                    block,
                    Terminator::new(TerminatorKind::Return(result), origin),
                )
                .expect("return");
        }
    }
    let checked = builder.finish_checked().expect("valid paired sum CFG");
    let dump = loom_codegen_ir::dump_program(&checked);
    assert!(
        dump.contains("left.payload0, left.payload1, right.payload0, right.payload1;"),
        "{dump}"
    );
    assert!(dump.contains("mismatch =>"), "{dump}");
}

#[test]
fn malformed_sum_construction_and_switch_are_rejected_independently() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let sum = builder
        .add_sum_type(nominal(30), &[Box::from([Type::Int]), Box::new([])])
        .expect("sum");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let origin = Origin::synthetic(FunctionId(30));
    let root = builder
        .declare_function(
            origin,
            "sum.switch.malformed",
            Signature::new(Vec::new(), unit),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = builder.function(root).expect("builder");
        let entry = function.create_block().expect("entry");
        let only_case = function.create_block().expect("case");
        function.set_entry(entry).expect("set entry");
        function
            .append_block_parameter(only_case, boolean)
            .expect("wrong payload parameter");
        let wrong = function
            .append_instruction(
                entry,
                InstructionKind::Constant(loom_codegen_ir::Constant::Bool(true)),
                &[boolean],
                origin,
            )
            .expect("Bool")[0];
        let sum_value = function
            .append_instruction(
                entry,
                InstructionKind::SumConstruct {
                    variant: 0,
                    payload: Box::from([wrong]),
                },
                &[sum],
                origin,
            )
            .expect("unchecked construction")[0];
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::SumSwitch {
                        scrutinee: sum_value,
                        cases: Box::from([SumCase::new(1, only_case, [])]),
                    },
                    origin,
                ),
            )
            .expect("unchecked switch");
        let result = function
            .append_instruction(
                only_case,
                InstructionKind::Constant(loom_codegen_ir::Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit")[0];
        function
            .terminate(
                only_case,
                Terminator::new(TerminatorKind::Return(result), origin),
            )
            .expect("return");
    }

    let errors = validate_program(&builder.finish()).expect_err("malformed sums must fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.path().contains("payload[0]")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape && error.path().contains("cases")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape && error.path().contains("case[0].variant")
    }));
}

#[test]
fn forged_float_parse_status_result_is_rejected_at_the_typed_boundary() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let text = builder
        .add_immortal_text_type()
        .expect("register canonical Text");
    let parse_error_semantic = nominal(31);
    let _parse_error = builder
        .add_sum_type(parse_error_semantic.clone(), &[Box::new([]), Box::new([])])
        .expect("ParseFloatError");
    let result = builder
        .add_sum_type(
            Type::Nominal(TypeId(32), vec![Type::Float, parse_error_semantic.clone()]),
            &[Box::from([Type::Float]), Box::from([parse_error_semantic])],
        )
        .expect("Result[Float, ParseFloatError]");
    let origin = Origin::synthetic(FunctionId(31));
    let root = builder
        .declare_function(
            origin,
            "parse.forged_status_variants",
            Signature::new(Vec::new(), result),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = builder.function(root).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let input = function
            .append_instruction(
                entry,
                InstructionKind::TextLiteral { utf8: "1".into() },
                &[text],
                origin,
            )
            .expect("input")[0];
        let parsed = function
            .append_instruction(
                entry,
                InstructionKind::FloatParseStatus { text: input },
                &[result],
                origin,
            )
            .expect("unchecked forged parse")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(parsed), origin),
            )
            .expect("return");
    }

    let errors = validate_program(&builder.finish()).expect_err("forged parse result must fail");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch && error.path().contains("result")
    }));
}

#[test]
fn aggregate_validation_scales_with_uses_without_copying_schemas() {
    const VARIANTS: usize = 128;
    const FIELDS: usize = 64;
    const USES: usize = 4_096;

    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let sum = builder
        .add_sum_type(nominal(35), &vec![Box::<[Type]>::default(); VARIANTS])
        .expect("wide tag-only sum");
    let product = builder
        .add_tuple_type(&vec![Type::Int; FIELDS])
        .expect("wide product");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let origin = Origin::synthetic(FunctionId(35));
    let root = builder
        .declare_function(
            origin,
            "aggregate.validation.scale",
            Signature::new(Vec::new(), unit),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = builder.function(root).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let mut fields = Vec::with_capacity(FIELDS);
        for value in 0..FIELDS {
            fields.push(
                function
                    .append_instruction(
                        entry,
                        InstructionKind::Constant(loom_codegen_ir::Constant::Int(
                            i64::try_from(value).expect("field value"),
                        )),
                        &[integer],
                        origin,
                    )
                    .expect("integer")[0],
            );
        }
        for _ in 0..USES {
            function
                .append_instruction(
                    entry,
                    InstructionKind::SumConstruct {
                        variant: 0,
                        payload: Box::new([]),
                    },
                    &[sum],
                    origin,
                )
                .expect("sum use");
            function
                .append_instruction(
                    entry,
                    InstructionKind::ProductConstruct {
                        fields: fields.clone().into_boxed_slice(),
                    },
                    &[product],
                    origin,
                )
                .expect("product use");
        }
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

    validate_program(&builder.finish()).expect(
        "validator must borrow aggregate metadata or select one variant instead of cloning schemas per use",
    );
}

#[test]
fn result_test_roots_require_an_explicit_checked_outcome_plan() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let result_semantic = nominal(40);
    let result = builder
        .add_sum_type(
            result_semantic,
            &[Box::from([Type::Unit]), Box::from([Type::Unit])],
        )
        .expect("Result-like sum");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let origin = Origin::synthetic(FunctionId(40));
    let root = builder
        .declare_function(
            origin,
            "result.test",
            Signature::new(Vec::new(), result),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = builder.function(root).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(loom_codegen_ir::Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit")[0];
        let outcome = function
            .append_instruction(
                entry,
                InstructionKind::SumConstruct {
                    variant: 0,
                    payload: Box::from([value]),
                },
                &[result],
                origin,
            )
            .expect("Ok(Unit)")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(outcome), origin),
            )
            .expect("return");
    }
    let program = builder.finish_checked().expect("valid result program");
    let artifact = program
        .clone()
        .into_artifact(ArtifactRootRequest::planned_tests([(
            root,
            TestOutcomePlan::Result {
                success_variant: 0,
                failure_variant: 1,
            },
        )]))
        .expect("explicit Result[Unit, E] plan");
    assert_eq!(
        artifact.test_outcomes(),
        Some(
            [TestOutcomePlan::Result {
                success_variant: 0,
                failure_variant: 1,
            }]
            .as_slice()
        )
    );
    let reversed = program
        .clone()
        .into_artifact(ArtifactRootRequest::planned_tests([(
            root,
            TestOutcomePlan::Result {
                success_variant: 1,
                failure_variant: 0,
            },
        )]))
        .expect("structurally valid reversed plan");
    assert_ne!(
        artifact_identity(&artifact),
        artifact_identity(&reversed),
        "test harness outcome semantics must be checked-artifact identity"
    );

    let errors = program
        .into_artifact(ArtifactRootRequest::tests([root]))
        .expect_err("a Result root cannot use the Unit outcome plan");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ArtifactValidationCode::RootSignature && error.path() == "roots.tests[0]"
    }));
}
