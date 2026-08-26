use loom_codegen_ir::{
    BlockTarget, Effects, InstructionKind, IntPredicate, Origin, Program, ProgramBuilder,
    Signature, TargetLayout, Terminator, TerminatorKind, ValidationCode, dump_program,
};
use loom_mir::{FunctionId as MirFunctionId, Type};

fn origin() -> Origin {
    Origin::synthetic(MirFunctionId(80))
}

fn terminator(kind: TerminatorKind) -> Terminator {
    Terminator::new(kind, origin())
}

#[derive(Clone, Copy)]
enum ProofShape {
    TrueEdge,
    NestedTrueRegion,
    NoBranch,
    UnreachableProofBranch,
    FalseEdge,
    Join,
    ExtraTruePredecessor,
    SameTarget,
    AmbiguousBranch,
    CopiedProof,
    WrongPredicate,
    WrongValue,
    WrongUpperBound,
    UnreachablePredecessor,
}

#[allow(clippy::too_many_lines)]
fn proof_program(shape: ProofShape) -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(),
            "proof.successor",
            Signature::new(vec![int_ty, int_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare proof function");
    {
        let mut function = program.function(function).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, int_ty)
            .expect("value");
        let upper_bound = function
            .append_block_parameter(entry, int_ty)
            .expect("upper bound");
        let predicate = if matches!(shape, ProofShape::WrongPredicate) {
            IntPredicate::LessEqual
        } else {
            IntPredicate::Less
        };
        let proof = function
            .append_instruction(
                entry,
                InstructionKind::IntCompare {
                    predicate,
                    left: value,
                    right: upper_bound,
                },
                &[bool_ty],
                origin(),
            )
            .expect("comparison")[0];
        let successor_value = if matches!(shape, ProofShape::WrongValue) {
            function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(loom_codegen_ir::Constant::Int(0)),
                    &[int_ty],
                    origin(),
                )
                .expect("wrong value")[0]
        } else {
            value
        };
        let successor_upper_bound = if matches!(shape, ProofShape::WrongUpperBound) {
            function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(loom_codegen_ir::Constant::Int(i64::MAX)),
                    &[int_ty],
                    origin(),
                )
                .expect("wrong upper bound")[0]
        } else {
            upper_bound
        };

        if matches!(shape, ProofShape::NoBranch) {
            let successor = function
                .append_instruction(
                    entry,
                    InstructionKind::IntSuccessorBelow {
                        value: successor_value,
                        upper_bound: successor_upper_bound,
                        proof,
                    },
                    &[int_ty],
                    origin(),
                )
                .expect("successor")[0];
            function
                .terminate(entry, terminator(TerminatorKind::Return(successor)))
                .expect("return");
            return program.finish();
        }
        if matches!(shape, ProofShape::UnreachableProofBranch) {
            let dead_branch = function.create_block().expect("dead branch");
            let dead_true = function.create_block().expect("dead true");
            let dead_false = function.create_block().expect("dead false");
            let successor = function
                .append_instruction(
                    entry,
                    InstructionKind::IntSuccessorBelow {
                        value,
                        upper_bound,
                        proof,
                    },
                    &[int_ty],
                    origin(),
                )
                .expect("unproved successor")[0];
            function
                .terminate(entry, terminator(TerminatorKind::Return(successor)))
                .expect("entry return");
            function
                .terminate(
                    dead_branch,
                    terminator(TerminatorKind::Branch {
                        condition: proof,
                        then_target: BlockTarget::new(dead_true, []),
                        else_target: BlockTarget::new(dead_false, []),
                    }),
                )
                .expect("unreachable proof branch");
            function
                .terminate(dead_true, terminator(TerminatorKind::Return(value)))
                .expect("dead true return");
            function
                .terminate(dead_false, terminator(TerminatorKind::Return(value)))
                .expect("dead false return");
            return program.finish();
        }

        let true_block = function.create_block().expect("true block");
        let false_block = function.create_block().expect("false block");
        function
            .terminate(
                entry,
                terminator(TerminatorKind::Branch {
                    condition: proof,
                    then_target: BlockTarget::new(true_block, []),
                    else_target: BlockTarget::new(
                        if matches!(shape, ProofShape::SameTarget) {
                            true_block
                        } else {
                            false_block
                        },
                        [],
                    ),
                }),
            )
            .expect("proof branch");

        match shape {
            ProofShape::TrueEdge
            | ProofShape::WrongPredicate
            | ProofShape::WrongValue
            | ProofShape::WrongUpperBound
            | ProofShape::SameTarget => {
                let successor = function
                    .append_instruction(
                        true_block,
                        InstructionKind::IntSuccessorBelow {
                            value: successor_value,
                            upper_bound: successor_upper_bound,
                            proof,
                        },
                        &[int_ty],
                        origin(),
                    )
                    .expect("successor")[0];
                function
                    .terminate(true_block, terminator(TerminatorKind::Return(successor)))
                    .expect("true return");
                function
                    .terminate(false_block, terminator(TerminatorKind::Return(value)))
                    .expect("false return");
            }
            ProofShape::NestedTrueRegion => {
                let nested = function.create_block().expect("nested block");
                function
                    .terminate(
                        true_block,
                        terminator(TerminatorKind::Jump(BlockTarget::new(nested, []))),
                    )
                    .expect("enter nested region");
                let successor = function
                    .append_instruction(
                        nested,
                        InstructionKind::IntSuccessorBelow {
                            value,
                            upper_bound,
                            proof,
                        },
                        &[int_ty],
                        origin(),
                    )
                    .expect("nested successor")[0];
                function
                    .terminate(nested, terminator(TerminatorKind::Return(successor)))
                    .expect("nested return");
                function
                    .terminate(false_block, terminator(TerminatorKind::Return(value)))
                    .expect("false return");
            }
            ProofShape::FalseEdge => {
                let successor = function
                    .append_instruction(
                        false_block,
                        InstructionKind::IntSuccessorBelow {
                            value,
                            upper_bound,
                            proof,
                        },
                        &[int_ty],
                        origin(),
                    )
                    .expect("false successor")[0];
                function
                    .terminate(true_block, terminator(TerminatorKind::Return(value)))
                    .expect("true return");
                function
                    .terminate(false_block, terminator(TerminatorKind::Return(successor)))
                    .expect("false return");
            }
            ProofShape::Join | ProofShape::CopiedProof => {
                let join = function.create_block().expect("join");
                let joined_proof = matches!(shape, ProofShape::CopiedProof).then(|| {
                    function
                        .append_block_parameter(join, bool_ty)
                        .expect("copied proof")
                });
                function
                    .terminate(
                        true_block,
                        terminator(TerminatorKind::Jump(BlockTarget::new(
                            join,
                            joined_proof.map_or_else(Vec::new, |_| vec![proof]),
                        ))),
                    )
                    .expect("true join");
                function
                    .terminate(
                        false_block,
                        terminator(TerminatorKind::Jump(BlockTarget::new(
                            join,
                            joined_proof.map_or_else(Vec::new, |_| vec![proof]),
                        ))),
                    )
                    .expect("false join");
                let successor = function
                    .append_instruction(
                        join,
                        InstructionKind::IntSuccessorBelow {
                            value,
                            upper_bound,
                            proof: joined_proof.unwrap_or(proof),
                        },
                        &[int_ty],
                        origin(),
                    )
                    .expect("joined successor")[0];
                function
                    .terminate(join, terminator(TerminatorKind::Return(successor)))
                    .expect("join return");
            }
            ProofShape::ExtraTruePredecessor => {
                let detour = function.create_block().expect("detour");
                function
                    .terminate(
                        false_block,
                        terminator(TerminatorKind::Jump(BlockTarget::new(detour, []))),
                    )
                    .expect("false detour");
                function
                    .terminate(
                        detour,
                        terminator(TerminatorKind::Jump(BlockTarget::new(true_block, []))),
                    )
                    .expect("extra predecessor");
                let successor = function
                    .append_instruction(
                        true_block,
                        InstructionKind::IntSuccessorBelow {
                            value,
                            upper_bound,
                            proof,
                        },
                        &[int_ty],
                        origin(),
                    )
                    .expect("successor")[0];
                function
                    .terminate(true_block, terminator(TerminatorKind::Return(successor)))
                    .expect("return");
            }
            ProofShape::AmbiguousBranch => {
                let second_true = function.create_block().expect("second true");
                let second_false = function.create_block().expect("second false");
                function
                    .terminate(
                        false_block,
                        terminator(TerminatorKind::Branch {
                            condition: proof,
                            then_target: BlockTarget::new(second_true, []),
                            else_target: BlockTarget::new(second_false, []),
                        }),
                    )
                    .expect("second proof branch");
                let successor = function
                    .append_instruction(
                        true_block,
                        InstructionKind::IntSuccessorBelow {
                            value,
                            upper_bound,
                            proof,
                        },
                        &[int_ty],
                        origin(),
                    )
                    .expect("successor")[0];
                function
                    .terminate(true_block, terminator(TerminatorKind::Return(successor)))
                    .expect("true return");
                function
                    .terminate(second_true, terminator(TerminatorKind::Return(value)))
                    .expect("second true return");
                function
                    .terminate(second_false, terminator(TerminatorKind::Return(value)))
                    .expect("second false return");
            }
            ProofShape::UnreachablePredecessor => {
                let unreachable = function.create_block().expect("unreachable");
                function
                    .terminate(
                        unreachable,
                        terminator(TerminatorKind::Jump(BlockTarget::new(true_block, []))),
                    )
                    .expect("unreachable predecessor");
                let successor = function
                    .append_instruction(
                        true_block,
                        InstructionKind::IntSuccessorBelow {
                            value,
                            upper_bound,
                            proof,
                        },
                        &[int_ty],
                        origin(),
                    )
                    .expect("successor")[0];
                function
                    .terminate(true_block, terminator(TerminatorKind::Return(successor)))
                    .expect("true return");
                function
                    .terminate(false_block, terminator(TerminatorKind::Return(value)))
                    .expect("false return");
            }
            ProofShape::NoBranch | ProofShape::UnreachableProofBranch => {
                function
                    .terminate(true_block, terminator(TerminatorKind::Return(value)))
                    .expect("defensive true return");
                function
                    .terminate(false_block, terminator(TerminatorKind::Return(value)))
                    .expect("defensive false return");
            }
        }
    }
    program.finish()
}

fn error_codes(program: Program) -> Vec<ValidationCode> {
    program
        .into_checked()
        .expect_err("invalid integer proof must be rejected")
        .as_slice()
        .iter()
        .map(loom_codegen_ir::ValidationError::code)
        .collect()
}

#[test]
fn exact_true_edge_proves_successor_in_nested_regions() {
    for shape in [ProofShape::TrueEdge, ProofShape::NestedTrueRegion] {
        let checked = proof_program(shape)
            .into_checked()
            .expect("true edge proves successor");
        let dump = dump_program(&checked);
        assert!(dump.contains("effects=none"), "{dump}");
        assert!(
            dump.contains("int.successor_below %v0, upper %v1, proof %v2"),
            "{dump}"
        );
    }
}

#[test]
fn proof_must_match_the_exact_comparison_and_reachable_true_edge() {
    for shape in [
        ProofShape::NoBranch,
        ProofShape::UnreachableProofBranch,
        ProofShape::FalseEdge,
        ProofShape::Join,
        ProofShape::ExtraTruePredecessor,
        ProofShape::SameTarget,
        ProofShape::AmbiguousBranch,
        ProofShape::CopiedProof,
        ProofShape::WrongPredicate,
        ProofShape::WrongValue,
        ProofShape::WrongUpperBound,
    ] {
        let codes = error_codes(proof_program(shape));
        assert!(
            codes.contains(&ValidationCode::InvalidIntegerProof),
            "shape did not report an integer-proof failure: {codes:?}"
        );
    }
}

#[test]
fn signed_minimum_and_maximum_bounds_are_safe() {
    for (start, upper_bound) in [(i64::MIN, i64::MIN), (i64::MAX - 1, i64::MAX)] {
        let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
        let int_ty = program.type_id(&Type::Int).expect("Int type");
        let function = program
            .declare_function(
                origin(),
                "proof.extreme",
                Signature::new(Vec::new(), int_ty),
                Effects::NONE,
            )
            .expect("declare");
        {
            let mut function = program.function(function).expect("function builder");
            let entry = function.create_block().expect("entry");
            let below = function.create_block().expect("below");
            let not_below = function.create_block().expect("not below");
            function.set_entry(entry).expect("set entry");
            let value = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(loom_codegen_ir::Constant::Int(start)),
                    &[int_ty],
                    origin(),
                )
                .expect("start")[0];
            let bound = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(loom_codegen_ir::Constant::Int(upper_bound)),
                    &[int_ty],
                    origin(),
                )
                .expect("bound")[0];
            let proof = function
                .append_instruction(
                    entry,
                    InstructionKind::IntCompare {
                        predicate: IntPredicate::Less,
                        left: value,
                        right: bound,
                    },
                    &[bool_ty],
                    origin(),
                )
                .expect("compare")[0];
            function
                .terminate(
                    entry,
                    terminator(TerminatorKind::Branch {
                        condition: proof,
                        then_target: BlockTarget::new(below, []),
                        else_target: BlockTarget::new(not_below, []),
                    }),
                )
                .expect("branch");
            let successor = function
                .append_instruction(
                    below,
                    InstructionKind::IntSuccessorBelow {
                        value,
                        upper_bound: bound,
                        proof,
                    },
                    &[int_ty],
                    origin(),
                )
                .expect("successor")[0];
            function
                .terminate(below, terminator(TerminatorKind::Return(successor)))
                .expect("below return");
            function
                .terminate(not_below, terminator(TerminatorKind::Return(value)))
                .expect("not-below return");
        }
        program
            .finish_checked()
            .expect("signed extreme comparison is a valid proof");
    }
}

#[test]
fn many_consumers_and_incoming_edges_are_validated_in_linear_tables() {
    const COUNT: usize = 4_096;

    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(),
            "proof.hostile_fan_in",
            Signature::new(vec![int_ty, int_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("function builder");
        let entry = function.create_block().expect("entry");
        let target = function.create_block().expect("target");
        let exit = function.create_block().expect("exit");
        let detours = (0..COUNT)
            .map(|_| function.create_block().expect("detour"))
            .collect::<Vec<_>>();
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, int_ty)
            .expect("value");
        let bound = function
            .append_block_parameter(entry, int_ty)
            .expect("bound");
        let proof = function
            .append_instruction(
                entry,
                InstructionKind::IntCompare {
                    predicate: IntPredicate::Less,
                    left: value,
                    right: bound,
                },
                &[bool_ty],
                origin(),
            )
            .expect("proof")[0];
        function
            .terminate(
                entry,
                terminator(TerminatorKind::Branch {
                    condition: proof,
                    then_target: BlockTarget::new(target, []),
                    else_target: BlockTarget::new(detours[0], []),
                }),
            )
            .expect("entry branch");
        for (index, detour) in detours.iter().copied().enumerate() {
            let condition = function
                .append_instruction(
                    detour,
                    InstructionKind::Constant(loom_codegen_ir::Constant::Bool(true)),
                    &[bool_ty],
                    origin(),
                )
                .expect("detour condition")[0];
            function
                .terminate(
                    detour,
                    terminator(TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(target, []),
                        else_target: BlockTarget::new(
                            detours.get(index + 1).copied().unwrap_or(exit),
                            [],
                        ),
                    }),
                )
                .expect("detour branch");
        }
        let mut last = value;
        for _ in 0..COUNT {
            last = function
                .append_instruction(
                    target,
                    InstructionKind::IntSuccessorBelow {
                        value,
                        upper_bound: bound,
                        proof,
                    },
                    &[int_ty],
                    origin(),
                )
                .expect("successor consumer")[0];
        }
        function
            .terminate(target, terminator(TerminatorKind::Return(last)))
            .expect("target return");
        function
            .terminate(exit, terminator(TerminatorKind::Return(value)))
            .expect("exit return");
    }

    let codes = error_codes(program.finish());
    assert!(
        codes.contains(&ValidationCode::InvalidIntegerProof),
        "fan-in must invalidate every proof consumer: {codes:?}"
    );
}

#[test]
fn unreachable_predecessors_do_not_invalidate_a_reachable_edge_proof() {
    let codes = error_codes(proof_program(ProofShape::UnreachablePredecessor));
    assert!(
        codes.contains(&ValidationCode::UnreachableBlock),
        "{codes:?}"
    );
    assert!(
        !codes.contains(&ValidationCode::InvalidIntegerProof),
        "unreachable CFG input must not invalidate the proof: {codes:?}"
    );
}

#[test]
fn an_unreachable_branch_cannot_establish_a_reachable_proof() {
    let codes = error_codes(proof_program(ProofShape::UnreachableProofBranch));
    assert!(
        codes.contains(&ValidationCode::UnreachableBlock),
        "{codes:?}"
    );
    assert!(
        codes.contains(&ValidationCode::InvalidIntegerProof),
        "an unreachable branch must not establish the fact: {codes:?}"
    );
}

#[test]
fn high_bit_loop_successor_is_pure_and_valid_on_its_backedge() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(),
            "proof.high_bit_loop",
            Signature::new(Vec::new(), int_ty),
            Effects::NONE,
        )
        .expect("declare loop");
    {
        let mut function = program.function(function).expect("function builder");
        let entry = function.create_block().expect("entry");
        let header = function.create_block().expect("header");
        let body = function.create_block().expect("body");
        let exit = function.create_block().expect("exit");
        function.set_entry(entry).expect("set entry");
        let start = function
            .append_instruction(
                entry,
                InstructionKind::Constant(loom_codegen_ir::Constant::Int(i64::MAX - 1)),
                &[int_ty],
                origin(),
            )
            .expect("start")[0];
        let bound = function
            .append_instruction(
                entry,
                InstructionKind::Constant(loom_codegen_ir::Constant::Int(i64::MAX)),
                &[int_ty],
                origin(),
            )
            .expect("bound")[0];
        let current = function
            .append_block_parameter(header, int_ty)
            .expect("current");
        function
            .terminate(
                entry,
                terminator(TerminatorKind::Jump(BlockTarget::new(header, vec![start]))),
            )
            .expect("enter loop");
        let proof = function
            .append_instruction(
                header,
                InstructionKind::IntCompare {
                    predicate: IntPredicate::Less,
                    left: current,
                    right: bound,
                },
                &[bool_ty],
                origin(),
            )
            .expect("proof")[0];
        function
            .terminate(
                header,
                terminator(TerminatorKind::Branch {
                    condition: proof,
                    then_target: BlockTarget::new(body, []),
                    else_target: BlockTarget::new(exit, []),
                }),
            )
            .expect("loop condition");
        let next = function
            .append_instruction(
                body,
                InstructionKind::IntSuccessorBelow {
                    value: current,
                    upper_bound: bound,
                    proof,
                },
                &[int_ty],
                origin(),
            )
            .expect("successor")[0];
        function
            .terminate(
                body,
                terminator(TerminatorKind::Jump(BlockTarget::new(header, vec![next]))),
            )
            .expect("backedge");
        function
            .terminate(exit, terminator(TerminatorKind::Return(current)))
            .expect("exit");
    }

    let checked = program.finish_checked().expect("valid high-bit loop");
    let dump = dump_program(&checked);
    assert!(dump.contains("const int 9223372036854775806"), "{dump}");
    assert!(dump.contains("const int 9223372036854775807"), "{dump}");
    assert!(dump.contains("effects=none"), "{dump}");
}
