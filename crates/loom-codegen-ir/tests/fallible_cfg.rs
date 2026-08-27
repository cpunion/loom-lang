use loom_codegen_ir::{
    BlockTarget, BoolPredicate, CheckedIntBinaryOp, Constant, ContractFaultMetadata, Effects,
    FaultMetadata, InstructionKind, Origin, Program, ProgramBuilder, ResultTarget, Signature,
    TargetLayout, Terminator, TerminatorKind, UnwindTarget, ValidationCode, dump_program,
};
use loom_mir::{FunctionId as MirFunctionId, Type};

fn origin(function: u32) -> Origin {
    Origin::synthetic(MirFunctionId(function))
}

fn terminator(function: u32, kind: TerminatorKind) -> Terminator {
    Terminator::new(kind, origin(function))
}

fn assertion_metadata(function: u32) -> ContractFaultMetadata {
    ContractFaultMetadata::assertion(origin(function).span)
}

fn assert_has_code(program: Program, expected: ValidationCode) {
    let errors = program
        .into_checked()
        .expect_err("malformed fallible LCIR must fail");
    assert!(
        errors
            .as_slice()
            .iter()
            .any(|error| error.code() == expected),
        "missing {expected:?}: {:#?}",
        errors.as_slice()
    );
}

#[test]
fn scalar_negation_and_boolean_comparison_are_typed_pure_instructions() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let float_ty = program.type_id(&Type::Float).expect("Float type");
    let function = program
        .declare_function(
            origin(70),
            "scalar.pure",
            Signature::new(vec![bool_ty, bool_ty, float_ty], float_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let left = function
            .append_block_parameter(entry, bool_ty)
            .expect("left");
        let right = function
            .append_block_parameter(entry, bool_ty)
            .expect("right");
        let value = function
            .append_block_parameter(entry, float_ty)
            .expect("value");
        function
            .append_instruction(
                entry,
                InstructionKind::BoolCompare {
                    predicate: BoolPredicate::NotEqual,
                    left,
                    right,
                },
                &[bool_ty],
                origin(70),
            )
            .expect("bool compare");
        let negated = function
            .append_instruction(
                entry,
                InstructionKind::FloatNegate { value },
                &[float_ty],
                origin(70),
            )
            .expect("float negate")[0];
        function
            .terminate(entry, terminator(70, TerminatorKind::Return(negated)))
            .expect("return");
    }

    let checked = program.finish_checked().expect("valid pure typed LCIR");
    let dump = dump_program(&checked);
    assert!(dump.contains("bool.compare.not_equal"));
    assert!(dump.contains("float.negate"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn fallible_results_exist_only_on_normal_edges_and_faults_resume_on_unwind_paths() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let divide = program
        .declare_function(
            origin(71),
            "fallible.divide",
            Signature::new(vec![int_ty, int_ty], int_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare divide");
    let checked_call = program
        .declare_function(
            origin(72),
            "fallible.checked_call",
            Signature::new(vec![bool_ty, int_ty, int_ty], int_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare caller");

    {
        let mut function = program.function(divide).expect("divide builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let fault = function.create_block().expect("fault");
        function.set_entry(entry).expect("set entry");
        let left = function
            .append_block_parameter(entry, int_ty)
            .expect("left");
        let right = function
            .append_block_parameter(entry, int_ty)
            .expect("right");
        let result = function
            .append_block_parameter(normal, int_ty)
            .expect("result");
        function
            .terminate(
                entry,
                terminator(
                    71,
                    TerminatorKind::CheckedIntBinary {
                        op: CheckedIntBinaryOp::Divide,
                        left,
                        right,
                        normal: ResultTarget::new(normal, Vec::new()),
                        fault: UnwindTarget::new(fault, Vec::new()),
                    },
                ),
            )
            .expect("checked divide");
        function
            .terminate(normal, terminator(71, TerminatorKind::Return(result)))
            .expect("return");
        function
            .terminate(fault, terminator(71, TerminatorKind::ResumeFault))
            .expect("resume fault");
    }

    {
        let mut function = program.function(checked_call).expect("caller builder");
        let entry = function.create_block().expect("entry");
        let after_assert = function.create_block().expect("after assert");
        let after_call = function.create_block().expect("after call");
        let returned = function.create_block().expect("returned");
        let assert_fault = function.create_block().expect("assert fault");
        let invoke_fault = function.create_block().expect("invoke fault");
        let negate_fault = function.create_block().expect("negate fault");
        let resume = function.create_block().expect("resume");
        function.set_entry(entry).expect("set entry");

        let condition = function
            .append_block_parameter(entry, bool_ty)
            .expect("condition");
        let left = function
            .append_block_parameter(entry, int_ty)
            .expect("left");
        let right = function
            .append_block_parameter(entry, int_ty)
            .expect("right");
        let asserted_left = function
            .append_block_parameter(after_assert, int_ty)
            .expect("asserted left");
        let asserted_right = function
            .append_block_parameter(after_assert, int_ty)
            .expect("asserted right");
        let quotient = function
            .append_block_parameter(after_call, int_ty)
            .expect("quotient");
        let _continued_condition = function
            .append_block_parameter(after_call, bool_ty)
            .expect("continued condition");
        let result = function
            .append_block_parameter(returned, int_ty)
            .expect("result");

        function
            .terminate(
                entry,
                terminator(
                    72,
                    TerminatorKind::Assert {
                        condition,
                        metadata: FaultMetadata::contract(assertion_metadata(72)),
                        success: BlockTarget::new(after_assert, vec![left, right]),
                        fault: UnwindTarget::new(assert_fault, Vec::new()),
                    },
                ),
            )
            .expect("assert");
        function
            .terminate(
                after_assert,
                terminator(
                    72,
                    TerminatorKind::Invoke {
                        callee: divide,
                        arguments: vec![asserted_left, asserted_right].into_boxed_slice(),
                        normal: ResultTarget::new(after_call, vec![condition]),
                        unwind: UnwindTarget::new(invoke_fault, Vec::new()),
                    },
                ),
            )
            .expect("invoke");
        function
            .terminate(
                after_call,
                terminator(
                    72,
                    TerminatorKind::CheckedIntNegate {
                        value: quotient,
                        normal: ResultTarget::new(returned, Vec::new()),
                        fault: UnwindTarget::new(negate_fault, Vec::new()),
                    },
                ),
            )
            .expect("checked negate");
        function
            .terminate(returned, terminator(72, TerminatorKind::Return(result)))
            .expect("return");
        for fault in [assert_fault, invoke_fault, negate_fault] {
            function
                .terminate(
                    fault,
                    terminator(
                        72,
                        TerminatorKind::Jump(BlockTarget::new(resume, Vec::new())),
                    ),
                )
                .expect("fault join");
        }
        function
            .terminate(resume, terminator(72, TerminatorKind::ResumeFault))
            .expect("resume fault");
    }

    let checked = program.finish_checked().expect("valid fallible typed LCIR");
    let first = dump_program(&checked);
    let second = dump_program(&checked);
    assert_eq!(first, second);
    for syntax in [
        "checked_int.divide %v0, %v1, normal b1(result), fault b2()",
        "assert %v0, contract AssertionFault category=assertion user_code=none message=\"assertion was not satisfied\" contract_span=file0:0..0 blame_span=file0:0..0, success b1(%v1, %v2), fault b4()",
        "invoke i0(%v3, %v4), normal b2(result; %v0), unwind b5()",
        "checked_int.negate %v5, normal b3(result), fault b6()",
        "resume_fault",
    ] {
        assert!(first.contains(syntax), "missing `{syntax}` in:\n{first}");
    }
}

#[test]
fn all_checked_integer_binary_operations_have_stable_distinct_dump_names() {
    for (index, (operation, name)) in [
        (CheckedIntBinaryOp::Add, "add"),
        (CheckedIntBinaryOp::Subtract, "subtract"),
        (CheckedIntBinaryOp::Multiply, "multiply"),
        (CheckedIntBinaryOp::Divide, "divide"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let int_ty = program.type_id(&Type::Int).expect("Int type");
        let source = u32::try_from(80 + index).expect("source");
        let function = program
            .declare_function(
                origin(source),
                format!("checked.{name}"),
                Signature::new(vec![int_ty, int_ty], int_ty),
                Effects::MAY_FAULT,
            )
            .expect("declare");
        {
            let mut function = program.function(function).expect("builder");
            let entry = function.create_block().expect("entry");
            let normal = function.create_block().expect("normal");
            let fault = function.create_block().expect("fault");
            function.set_entry(entry).expect("set entry");
            let left = function
                .append_block_parameter(entry, int_ty)
                .expect("left");
            let right = function
                .append_block_parameter(entry, int_ty)
                .expect("right");
            let result = function
                .append_block_parameter(normal, int_ty)
                .expect("result");
            function
                .terminate(
                    entry,
                    terminator(
                        source,
                        TerminatorKind::CheckedIntBinary {
                            op: operation,
                            left,
                            right,
                            normal: ResultTarget::new(normal, Vec::new()),
                            fault: UnwindTarget::new(fault, Vec::new()),
                        },
                    ),
                )
                .expect("checked operation");
            function
                .terminate(normal, terminator(source, TerminatorKind::Return(result)))
                .expect("return");
            function
                .terminate(fault, terminator(source, TerminatorKind::ResumeFault))
                .expect("resume");
        }
        let dump = dump_program(&program.finish_checked().expect("valid checked operation"));
        assert!(dump.contains(&format!("checked_int.{name}")));
    }
}

#[test]
fn exact_effects_are_required_and_call_forms_cannot_hide_faults() {
    assert_has_code(unnecessary_effect_program(), ValidationCode::EffectMismatch);
    assert_has_code(
        checked_without_effect_program(),
        ValidationCode::EffectMismatch,
    );
    assert_has_code(direct_call_to_faulting_program(), ValidationCode::CallShape);
    assert_has_code(invoke_infallible_program(), ValidationCode::CallShape);
}

#[test]
fn exact_effect_recomputation_closes_a_recursive_call_graph() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let first = program
        .declare_function(
            origin(103),
            "recursive.first",
            Signature::new(vec![bool_ty], unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare first");
    let second = program
        .declare_function(
            origin(104),
            "recursive.second",
            Signature::new(vec![bool_ty], unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare second");
    let third = program
        .declare_function(
            origin(105),
            "recursive.third",
            Signature::new(vec![bool_ty], unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare third");

    build_recursive_invoker(&mut program, first, 103, second, bool_ty, unit_ty, true);
    build_recursive_invoker(&mut program, second, 104, third, bool_ty, unit_ty, false);
    build_recursive_invoker(&mut program, third, 105, first, bool_ty, unit_ty, false);

    program
        .finish_checked()
        .expect("the whole recursive SCC has the exact transitive MAY_FAULT effect");
}

#[test]
fn an_infallible_recursive_cycle_keeps_the_minimal_empty_effect() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let first = program
        .declare_function(
            origin(106),
            "recursive.infallible.first",
            Signature::new(vec![unit_ty], unit_ty),
            Effects::NONE,
        )
        .expect("declare first");
    let second = program
        .declare_function(
            origin(107),
            "recursive.infallible.second",
            Signature::new(vec![unit_ty], unit_ty),
            Effects::NONE,
        )
        .expect("declare second");
    for (function_id, source, invoked_function) in [(first, 106, second), (second, 107, first)] {
        let mut function = program.function(function_id).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let argument = function
            .append_block_parameter(entry, unit_ty)
            .expect("argument");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: invoked_function,
                    arguments: vec![argument].into_boxed_slice(),
                },
                &[unit_ty],
                origin(source),
            )
            .expect("direct call")[0];
        function
            .terminate(entry, terminator(source, TerminatorKind::Return(result)))
            .expect("return");
    }

    program
        .finish_checked()
        .expect("an unseeded recursive SCC remains exactly infallible");
}

#[test]
fn invoke_only_recursive_cycles_cannot_self_justify_may_fault() {
    for (label, program) in [
        ("self recursive", invoke_only_cycle_program(1)),
        ("mutually recursive", invoke_only_cycle_program(2)),
        (
            "self recursive with an active-only checked cleanup",
            active_cleanup_only_cycle_program(),
        ),
    ] {
        let errors = program
            .into_checked()
            .expect_err("an invoke-only SCC has no real MAY_FAULT seed");
        for expected in [ValidationCode::EffectMismatch, ValidationCode::CallShape] {
            assert!(
                errors
                    .as_slice()
                    .iter()
                    .any(|error| error.code() == expected),
                "{label} cycle is missing {expected:?}: {:#?}",
                errors.as_slice()
            );
        }
    }
}

#[test]
fn fault_state_cannot_merge_or_cross_the_wrong_terminal_boundary() {
    assert_has_code(inactive_resume_program(), ValidationCode::FaultState);
    assert_has_code(active_return_program(), ValidationCode::FaultState);
    assert_has_code(mixed_fault_state_program(), ValidationCode::FaultState);
}

#[test]
#[allow(clippy::too_many_lines)]
fn active_cleanup_keeps_the_primary_fault_across_a_secondary_checked_operation() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(108),
            "cleanup.preserve_primary_fault",
            Signature::new(vec![bool_ty, int_ty], unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let cleanup = function.create_block().expect("cleanup");
        let cleanup_succeeded = function.create_block().expect("cleanup succeeded");
        let cleanup_faulted = function.create_block().expect("cleanup faulted");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_block_parameter(entry, bool_ty)
            .expect("condition");
        let cleanup_value = function
            .append_block_parameter(entry, int_ty)
            .expect("cleanup value");
        function
            .append_block_parameter(cleanup_succeeded, int_ty)
            .expect("ignored cleanup result");
        function
            .terminate(
                entry,
                terminator(
                    108,
                    TerminatorKind::Assert {
                        condition,
                        metadata: FaultMetadata::contract(assertion_metadata(108)),
                        success: BlockTarget::new(normal, Vec::new()),
                        fault: UnwindTarget::new(cleanup, Vec::new()),
                    },
                ),
            )
            .expect("assert");
        let unit = function
            .append_instruction(
                normal,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(108),
            )
            .expect("unit")[0];
        function
            .terminate(normal, terminator(108, TerminatorKind::Return(unit)))
            .expect("normal return");
        function
            .terminate(
                cleanup,
                terminator(
                    108,
                    TerminatorKind::CheckedIntNegate {
                        value: cleanup_value,
                        normal: ResultTarget::new(cleanup_succeeded, Vec::new()),
                        fault: UnwindTarget::new(cleanup_faulted, Vec::new()),
                    },
                ),
            )
            .expect("fallible cleanup");
        // Both outcomes remain active: success preserves the earlier primary
        // fault, while a secondary cleanup fault is suppressed rather than
        // replacing it. Remaining cleanup would be inserted before these
        // resume terminals by the later cleanup lowerer.
        for terminal in [cleanup_succeeded, cleanup_faulted] {
            function
                .terminate(terminal, terminator(108, TerminatorKind::ResumeFault))
                .expect("resume primary fault");
        }
    }

    program
        .finish_checked()
        .expect("fallible active cleanup preserves the primary fault state");
}

#[test]
fn result_and_unwind_edges_validate_implicit_result_shape_and_identity() {
    assert_has_code(
        missing_result_parameter_program(),
        ValidationCode::BlockArgument,
    );
    assert_has_code(
        duplicate_normal_unwind_program(),
        ValidationCode::DuplicateSuccessor,
    );
    assert_has_code(
        foreign_fallible_targets_program(),
        ValidationCode::InvalidBlockReference,
    );
}

#[allow(clippy::too_many_arguments)]
fn build_recursive_invoker(
    program: &mut ProgramBuilder,
    function_id: loom_codegen_ir::InstanceId,
    source: u32,
    invoked_function: loom_codegen_ir::InstanceId,
    bool_ty: loom_codegen_ir::ValueTypeId,
    unit_ty: loom_codegen_ir::ValueTypeId,
    originates_fault: bool,
) {
    let mut function = program.function(function_id).expect("builder");
    let entry = function.create_block().expect("entry");
    let invoke = function.create_block().expect("invoke");
    let normal = function.create_block().expect("normal");
    let unwind = function.create_block().expect("unwind");
    let fault = originates_fault.then(|| function.create_block().expect("fault"));
    function.set_entry(entry).expect("set entry");
    let condition = function
        .append_block_parameter(entry, bool_ty)
        .expect("condition");
    let result = function
        .append_block_parameter(normal, unit_ty)
        .expect("result");
    if let Some(fault) = fault {
        function
            .terminate(
                entry,
                terminator(
                    source,
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(invoke, Vec::new()),
                        else_target: BlockTarget::new(fault, Vec::new()),
                    },
                ),
            )
            .expect("branch");
        function
            .terminate(
                fault,
                terminator(
                    source,
                    TerminatorKind::Fault {
                        metadata: FaultMetadata::contract(assertion_metadata(source)),
                    },
                ),
            )
            .expect("fault");
    } else {
        function
            .terminate(
                entry,
                terminator(
                    source,
                    TerminatorKind::Jump(BlockTarget::new(invoke, Vec::new())),
                ),
            )
            .expect("jump");
    }
    function
        .terminate(
            invoke,
            terminator(
                source,
                TerminatorKind::Invoke {
                    callee: invoked_function,
                    arguments: vec![condition].into_boxed_slice(),
                    normal: ResultTarget::new(normal, Vec::new()),
                    unwind: UnwindTarget::new(unwind, Vec::new()),
                },
            ),
        )
        .expect("invoke");
    function
        .terminate(normal, terminator(source, TerminatorKind::Return(result)))
        .expect("return");
    function
        .terminate(unwind, terminator(source, TerminatorKind::ResumeFault))
        .expect("resume");
}

fn invoke_only_cycle_program(function_count: usize) -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let functions = (0..function_count)
        .map(|index| {
            let source = u32::try_from(109 + index).expect("source ID");
            program
                .declare_function(
                    origin(source),
                    format!("bad.invoke_only_cycle.{index}"),
                    Signature::new(vec![unit_ty], unit_ty),
                    Effects::MAY_FAULT,
                )
                .expect("declare cycle function")
        })
        .collect::<Vec<_>>();
    for (index, function_id) in functions.iter().copied().enumerate() {
        let source = u32::try_from(109 + index).expect("source ID");
        let invoked_function = functions[(index + 1) % functions.len()];
        let mut function = program.function(function_id).expect("builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let unwind = function.create_block().expect("unwind");
        function.set_entry(entry).expect("set entry");
        let argument = function
            .append_block_parameter(entry, unit_ty)
            .expect("argument");
        let result = function
            .append_block_parameter(normal, unit_ty)
            .expect("result");
        function
            .terminate(
                entry,
                terminator(
                    source,
                    TerminatorKind::Invoke {
                        callee: invoked_function,
                        arguments: vec![argument].into_boxed_slice(),
                        normal: ResultTarget::new(normal, Vec::new()),
                        unwind: UnwindTarget::new(unwind, Vec::new()),
                    },
                ),
            )
            .expect("invoke");
        function
            .terminate(normal, terminator(source, TerminatorKind::Return(result)))
            .expect("return");
        function
            .terminate(unwind, terminator(source, TerminatorKind::ResumeFault))
            .expect("resume");
    }
    program.finish()
}

fn active_cleanup_only_cycle_program() -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function_id = program
        .declare_function(
            origin(120),
            "bad.active_cleanup_only_cycle",
            Signature::new(vec![int_ty], int_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare cycle function");
    {
        let mut function = program.function(function_id).expect("builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let cleanup = function.create_block().expect("active cleanup");
        let cleanup_normal = function.create_block().expect("cleanup normal");
        let cleanup_fault = function.create_block().expect("cleanup fault");
        function.set_entry(entry).expect("set entry");
        let argument = function
            .append_block_parameter(entry, int_ty)
            .expect("argument");
        let result = function
            .append_block_parameter(normal, int_ty)
            .expect("result");
        function
            .append_block_parameter(cleanup_normal, int_ty)
            .expect("suppressed cleanup result");
        function
            .terminate(
                entry,
                terminator(
                    120,
                    TerminatorKind::Invoke {
                        callee: function_id,
                        arguments: vec![argument].into_boxed_slice(),
                        normal: ResultTarget::new(normal, Vec::new()),
                        unwind: UnwindTarget::new(cleanup, Vec::new()),
                    },
                ),
            )
            .expect("invoke");
        function
            .terminate(normal, terminator(120, TerminatorKind::Return(result)))
            .expect("return");
        function
            .terminate(
                cleanup,
                terminator(
                    120,
                    TerminatorKind::CheckedIntNegate {
                        value: argument,
                        normal: ResultTarget::new(cleanup_normal, Vec::new()),
                        fault: UnwindTarget::new(cleanup_fault, Vec::new()),
                    },
                ),
            )
            .expect("checked active cleanup");
        for block in [cleanup_normal, cleanup_fault] {
            function
                .terminate(block, terminator(120, TerminatorKind::ResumeFault))
                .expect("resume primary fault");
        }
    }
    program.finish()
}

fn unnecessary_effect_program() -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(90),
            "bad.unnecessary_effect",
            Signature::new(Vec::new(), unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(90),
            )
            .expect("unit")[0];
        function
            .terminate(entry, terminator(90, TerminatorKind::Return(unit)))
            .expect("return");
    }
    program.finish()
}

fn checked_without_effect_program() -> Program {
    checked_negate_program(91, Effects::NONE, false)
}

fn inactive_resume_program() -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(92),
            "bad.inactive_resume",
            Signature::new(Vec::new(), unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        function
            .terminate(entry, terminator(92, TerminatorKind::ResumeFault))
            .expect("resume");
    }
    program.finish()
}

fn active_return_program() -> Program {
    checked_negate_program(93, Effects::MAY_FAULT, true)
}

fn checked_negate_program(source: u32, effects: Effects, return_from_fault: bool) -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(source),
            "bad.checked_negate",
            Signature::new(vec![int_ty], int_ty),
            effects,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let fault = function.create_block().expect("fault");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, int_ty)
            .expect("value");
        let result = function
            .append_block_parameter(normal, int_ty)
            .expect("result");
        function
            .terminate(
                entry,
                terminator(
                    source,
                    TerminatorKind::CheckedIntNegate {
                        value,
                        normal: ResultTarget::new(normal, Vec::new()),
                        fault: UnwindTarget::new(fault, Vec::new()),
                    },
                ),
            )
            .expect("checked negate");
        function
            .terminate(normal, terminator(source, TerminatorKind::Return(result)))
            .expect("return");
        let fault_terminator = if return_from_fault {
            TerminatorKind::Return(value)
        } else {
            TerminatorKind::ResumeFault
        };
        function
            .terminate(fault, terminator(source, fault_terminator))
            .expect("fault terminal");
    }
    program.finish()
}

#[allow(clippy::too_many_lines)]
fn mixed_fault_state_program() -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(94),
            "bad.mixed_fault_state",
            Signature::new(vec![bool_ty], unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let plain = function.create_block().expect("plain");
        let asserting = function.create_block().expect("asserting");
        let success = function.create_block().expect("success");
        let merge = function.create_block().expect("merge");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_block_parameter(entry, bool_ty)
            .expect("condition");
        function
            .terminate(
                entry,
                terminator(
                    94,
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(plain, Vec::new()),
                        else_target: BlockTarget::new(asserting, Vec::new()),
                    },
                ),
            )
            .expect("branch");
        function
            .terminate(
                plain,
                terminator(
                    94,
                    TerminatorKind::Jump(BlockTarget::new(merge, Vec::new())),
                ),
            )
            .expect("plain jump");
        function
            .terminate(
                asserting,
                terminator(
                    94,
                    TerminatorKind::Assert {
                        condition,
                        metadata: FaultMetadata::contract(assertion_metadata(94)),
                        success: BlockTarget::new(success, Vec::new()),
                        fault: UnwindTarget::new(merge, Vec::new()),
                    },
                ),
            )
            .expect("assert");
        let unit = function
            .append_instruction(
                success,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(94),
            )
            .expect("unit")[0];
        function
            .terminate(success, terminator(94, TerminatorKind::Return(unit)))
            .expect("return");
        function
            .terminate(merge, terminator(94, TerminatorKind::ResumeFault))
            .expect("resume");
    }
    program.finish()
}

fn missing_result_parameter_program() -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(95),
            "bad.missing_result_parameter",
            Signature::new(vec![int_ty], int_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let fault = function.create_block().expect("fault");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, int_ty)
            .expect("value");
        function
            .terminate(
                entry,
                terminator(
                    95,
                    TerminatorKind::CheckedIntNegate {
                        value,
                        normal: ResultTarget::new(normal, Vec::new()),
                        fault: UnwindTarget::new(fault, Vec::new()),
                    },
                ),
            )
            .expect("checked negate");
        function
            .terminate(normal, terminator(95, TerminatorKind::Return(value)))
            .expect("return");
        function
            .terminate(fault, terminator(95, TerminatorKind::ResumeFault))
            .expect("resume");
    }
    program.finish()
}

fn duplicate_normal_unwind_program() -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(96),
            "bad.duplicate_normal_unwind",
            Signature::new(vec![int_ty], int_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let shared = function.create_block().expect("shared");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, int_ty)
            .expect("value");
        function
            .append_block_parameter(shared, int_ty)
            .expect("result or forwarded value");
        function
            .terminate(
                entry,
                terminator(
                    96,
                    TerminatorKind::CheckedIntNegate {
                        value,
                        normal: ResultTarget::new(shared, Vec::new()),
                        fault: UnwindTarget::new(shared, vec![value]),
                    },
                ),
            )
            .expect("checked negate");
        function
            .terminate(shared, terminator(96, TerminatorKind::ResumeFault))
            .expect("resume");
    }
    program.finish()
}

fn direct_call_to_faulting_program() -> Program {
    call_form_program(97, true)
}

fn invoke_infallible_program() -> Program {
    call_form_program(99, false)
}

#[allow(clippy::too_many_lines)]
fn call_form_program(source: u32, direct_to_faulting: bool) -> Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function_effects = if direct_to_faulting {
        Effects::MAY_FAULT
    } else {
        Effects::NONE
    };
    let invoked_function = program
        .declare_function(
            origin(source),
            "bad.call_form.callee",
            Signature::new(Vec::new(), unit_ty),
            function_effects,
        )
        .expect("declare callee");
    let wrapper = program
        .declare_function(
            origin(source + 1),
            "bad.call_form.caller",
            Signature::new(Vec::new(), unit_ty),
            function_effects,
        )
        .expect("declare caller");
    {
        let mut function = program.function(invoked_function).expect("callee builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        if direct_to_faulting {
            function
                .terminate(
                    entry,
                    terminator(
                        source,
                        TerminatorKind::Fault {
                            metadata: FaultMetadata::contract(assertion_metadata(source)),
                        },
                    ),
                )
                .expect("fault");
        } else {
            let unit = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit_ty],
                    origin(source),
                )
                .expect("unit")[0];
            function
                .terminate(entry, terminator(source, TerminatorKind::Return(unit)))
                .expect("return");
        }
    }
    {
        let mut function = program.function(wrapper).expect("caller builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        if direct_to_faulting {
            let result = function
                .append_instruction(
                    entry,
                    InstructionKind::DirectCall {
                        callee: invoked_function,
                        arguments: Vec::new().into_boxed_slice(),
                    },
                    &[unit_ty],
                    origin(source + 1),
                )
                .expect("direct call")[0];
            function
                .terminate(
                    entry,
                    terminator(source + 1, TerminatorKind::Return(result)),
                )
                .expect("return");
        } else {
            let normal = function.create_block().expect("normal");
            let unwind = function.create_block().expect("unwind");
            let result = function
                .append_block_parameter(normal, unit_ty)
                .expect("result");
            function
                .terminate(
                    entry,
                    terminator(
                        source + 1,
                        TerminatorKind::Invoke {
                            callee: invoked_function,
                            arguments: Vec::new().into_boxed_slice(),
                            normal: ResultTarget::new(normal, Vec::new()),
                            unwind: UnwindTarget::new(unwind, Vec::new()),
                        },
                    ),
                )
                .expect("invoke");
            function
                .terminate(
                    normal,
                    terminator(source + 1, TerminatorKind::Return(result)),
                )
                .expect("return");
            function
                .terminate(unwind, terminator(source + 1, TerminatorKind::ResumeFault))
                .expect("resume");
        }
    }
    program.finish()
}

#[allow(clippy::too_many_lines)]
fn foreign_fallible_targets_program() -> Program {
    let mut donor = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let donor_int = donor.type_id(&Type::Int).expect("Int type");
    let donor_function = donor
        .declare_function(
            origin(101),
            "donor",
            Signature::new(Vec::new(), donor_int),
            Effects::NONE,
        )
        .expect("declare donor");
    let (foreign_normal, foreign_fault) = {
        let mut function = donor.function(donor_function).expect("donor builder");
        let entry = function.create_block().expect("entry");
        let foreign_normal = function.create_block().expect("normal");
        let foreign_fault = function.create_block().expect("fault");
        function.set_entry(entry).expect("set entry");
        (foreign_normal, foreign_fault)
    };

    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(102),
            "bad.foreign_targets",
            Signature::new(vec![int_ty], int_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, int_ty)
            .expect("value");
        function
            .terminate(
                entry,
                terminator(
                    102,
                    TerminatorKind::CheckedIntNegate {
                        value,
                        normal: ResultTarget::new(foreign_normal, Vec::new()),
                        fault: UnwindTarget::new(foreign_fault, Vec::new()),
                    },
                ),
            )
            .expect("checked negate");
    }
    program.finish()
}
