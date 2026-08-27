use loom_codegen_ir::{
    BlockTarget, BuildErrorCode, Constant, Effects, FaultCode, FloatPredicate, InstructionKind,
    Origin, ProgramBuilder, Signature, TargetLayout, Terminator, TerminatorKind, ValidationCode,
    dump_program, write_program_with_options,
};
use loom_mir::{FunctionId as MirFunctionId, Type};

fn origin(function: u32) -> Origin {
    Origin::synthetic(MirFunctionId(function))
}

fn terminator(function: u32, kind: TerminatorKind) -> Terminator {
    Terminator::new(kind, origin(function))
}

#[test]
#[allow(clippy::too_many_lines)]
fn branch_merge_is_a_typed_block_parameter_and_dump_is_deterministic() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let choose = program
        .declare_function(
            origin(7),
            "example.choose",
            Signature::new(vec![bool_ty, int_ty, int_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare choose");

    {
        let mut function = program.function(choose).expect("choose builder");
        let entry = function.create_block().expect("entry");
        let then_block = function.create_block().expect("then");
        let else_block = function.create_block().expect("else");
        let join = function.create_block().expect("join");
        function.set_entry(entry).expect("set entry");

        let condition = function
            .append_block_parameter(entry, bool_ty)
            .expect("condition");
        let first = function
            .append_block_parameter(entry, int_ty)
            .expect("first");
        let second = function
            .append_block_parameter(entry, int_ty)
            .expect("second");
        let selected = function
            .append_block_parameter(join, int_ty)
            .expect("selected");

        function
            .terminate(
                entry,
                terminator(
                    7,
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(then_block, Vec::new()),
                        else_target: BlockTarget::new(else_block, Vec::new()),
                    },
                ),
            )
            .expect("branch");
        function
            .terminate(
                then_block,
                terminator(7, TerminatorKind::Jump(BlockTarget::new(join, vec![first]))),
            )
            .expect("then jump");
        function
            .terminate(
                else_block,
                terminator(
                    7,
                    TerminatorKind::Jump(BlockTarget::new(join, vec![second])),
                ),
            )
            .expect("else jump");
        function
            .terminate(join, terminator(7, TerminatorKind::Return(selected)))
            .expect("return");
    }

    let checked = program.finish_checked().expect("valid choose LCIR");
    let first_dump = dump_program(&checked);
    let second_dump = dump_program(&checked);
    assert_eq!(first_dump, second_dump);
    assert_eq!(
        first_dump,
        r#"lcir 5
target pointer_bits=64

repr r0 = uninhabited
repr r1 = zst
repr r2 = i1
repr r3 = i64
repr r4 = f64

type t0 = Never => r0
type t1 = Unit => r1
type t2 = Bool => r2
type t3 = Int => r3
type t4 = Float => r4

registration k0 = Never => t0
registration k1 = Unit => t1
registration k2 = Bool => t2
registration k3 = Int => t3
registration k4 = Float => t4

instance i0 = source=f7 types=[] witnesses=[]

fn i0 mir=f7 "example.choose" (t2, t3, t3) -> t3 entry=b0 effects=none {
  b0(%v0: t2, %v1: t3, %v2: t3):
    branch %v0, b1(), b2()

  b1:
    jump b3(%v1)

  b2:
    jump b3(%v2)

  b3(%v3: t3):
    return %v3
}
"#
    );

    let mut with_origins = String::new();
    write_program_with_options(
        &checked,
        loom_codegen_ir::DumpOptions {
            include_origins: true,
        },
        &mut with_origins,
    )
    .expect("write dump");
    assert!(with_origins.contains("function-origin f7 file0:0..0"));
    assert!(with_origins.contains("origin f7 file0:0..0"));
}

#[test]
fn validator_rejects_type_edge_dominance_and_termination_defects() {
    assert_has_code(wrong_return_program(), ValidationCode::ReturnType);
    assert_has_code(bad_edge_program(), ValidationCode::BlockArgument);
    assert_has_code(non_dominating_program(), ValidationCode::Dominance);
    assert_has_code(
        missing_terminator_program(),
        ValidationCode::MissingTerminator,
    );
    assert_has_code(
        entry_predecessor_program(),
        ValidationCode::EntryPredecessor,
    );
    assert_has_code(
        cross_function_value_program(),
        ValidationCode::InvalidValueReference,
    );
    assert_has_code(
        cross_function_block_program(),
        ValidationCode::InvalidBlockReference,
    );
    assert_has_code(
        uninhabited_call_result_program(),
        ValidationCode::UninhabitedValue,
    );
    assert_has_code(origin_mismatch_program(), ValidationCode::OriginMismatch);
    assert_has_code(missing_entry_program(), ValidationCode::MissingEntry);
    assert_has_code(entry_signature_program(), ValidationCode::EntrySignature);
    assert_has_code(
        unreachable_block_program(),
        ValidationCode::UnreachableBlock,
    );
    assert_has_code(
        instruction_result_shape_program(),
        ValidationCode::InstructionShape,
    );
    assert_has_code(invalid_call_program(), ValidationCode::CallShape);
}

#[test]
fn branch_edges_may_select_distinct_arguments_for_one_destination() {
    same_target_branch_program()
        .into_checked()
        .expect("branch edges are normalized independently by target backends");
}

#[test]
fn floating_not_equal_is_explicitly_unordered() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let float_ty = program.type_id(&Type::Float).expect("Float type");
    let function = program
        .declare_function(
            origin(20),
            "example.float_not_equal",
            Signature::new(vec![float_ty, float_ty], bool_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let left = function
            .append_block_parameter(entry, float_ty)
            .expect("left");
        let right = function
            .append_block_parameter(entry, float_ty)
            .expect("right");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::FloatCompare {
                    predicate: FloatPredicate::UnorderedNotEqual,
                    left,
                    right,
                },
                &[bool_ty],
                origin(20),
            )
            .expect("compare")[0];
        function
            .terminate(entry, terminator(20, TerminatorKind::Return(result)))
            .expect("return");
    }

    let checked = program.finish_checked().expect("valid float comparison");
    assert!(dump_program(&checked).contains("float.compare.unordered_not_equal"));
}

#[test]
fn loop_header_parameters_accept_a_valid_backedge() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(25),
            "example.loop_header",
            Signature::new(vec![bool_ty, int_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let header = function.create_block().expect("header");
        let body = function.create_block().expect("body");
        let exit = function.create_block().expect("exit");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_block_parameter(entry, bool_ty)
            .expect("condition");
        let initial = function
            .append_block_parameter(entry, int_ty)
            .expect("initial");
        let loop_condition = function
            .append_block_parameter(header, bool_ty)
            .expect("loop condition");
        let carried = function
            .append_block_parameter(header, int_ty)
            .expect("carried");
        let result = function
            .append_block_parameter(exit, int_ty)
            .expect("result");
        function
            .terminate(
                entry,
                terminator(
                    25,
                    TerminatorKind::Jump(BlockTarget::new(header, vec![condition, initial])),
                ),
            )
            .expect("enter loop");
        function
            .terminate(
                header,
                terminator(
                    25,
                    TerminatorKind::Branch {
                        condition: loop_condition,
                        then_target: BlockTarget::new(exit, vec![carried]),
                        else_target: BlockTarget::new(body, Vec::new()),
                    },
                ),
            )
            .expect("loop branch");
        function
            .terminate(
                body,
                terminator(
                    25,
                    TerminatorKind::Jump(BlockTarget::new(header, vec![loop_condition, carried])),
                ),
            )
            .expect("backedge");
        function
            .terminate(exit, terminator(25, TerminatorKind::Return(result)))
            .expect("return");
    }

    program
        .finish_checked()
        .expect("valid loop header and backedge");
}

#[test]
fn infallible_direct_call_uses_the_declared_scalar_signature() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let identity = program
        .declare_function(
            origin(26),
            "example.identity",
            Signature::new(vec![bool_ty], bool_ty),
            Effects::NONE,
        )
        .expect("declare callee");
    let call_identity = program
        .declare_function(
            origin(27),
            "example.call_identity",
            Signature::new(vec![bool_ty], bool_ty),
            Effects::NONE,
        )
        .expect("declare caller");
    {
        let mut function = program.function(identity).expect("callee builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, bool_ty)
            .expect("value");
        function
            .terminate(entry, terminator(26, TerminatorKind::Return(value)))
            .expect("return");
    }
    {
        let mut function = program.function(call_identity).expect("caller builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let argument = function
            .append_block_parameter(entry, bool_ty)
            .expect("argument");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: identity,
                    arguments: vec![argument].into_boxed_slice(),
                },
                &[bool_ty],
                origin(27),
            )
            .expect("call")[0];
        function
            .terminate(entry, terminator(27, TerminatorKind::Return(result)))
            .expect("return");
    }

    program
        .finish_checked()
        .expect("valid infallible direct call");
}

#[test]
#[allow(clippy::too_many_lines)]
fn generative_program_brands_reject_cross_program_ids_without_changing_dumps() {
    let (first_id, first_dump) = unit_identity_dump(28);
    let (second_id, second_dump) = unit_identity_dump(28);
    assert_ne!(first_id, second_id);
    assert_eq!(format!("{first_id:?}"), "i0");
    assert_eq!(format!("{second_id:?}"), "i0");
    assert_eq!(first_dump, second_dump);

    let mut donor = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let donor_bool = donor.type_id(&Type::Bool).expect("donor Bool type");
    let donor_unit = donor.type_id(&Type::Unit).expect("donor Unit type");
    let donor_function = donor
        .declare_function(
            origin(29),
            "donor.identity",
            Signature::new(vec![donor_bool], donor_bool),
            Effects::NONE,
        )
        .expect("declare donor");
    let donor_block_function = donor
        .declare_function(
            origin(30),
            "donor.unit",
            Signature::new(Vec::new(), donor_unit),
            Effects::NONE,
        )
        .expect("declare block donor");
    let donor_value = {
        let mut function = donor.function(donor_function).expect("donor builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, donor_bool)
            .expect("value");
        function
            .terminate(entry, terminator(29, TerminatorKind::Return(value)))
            .expect("return");
        value
    };
    let donor_block = {
        let mut function = donor
            .function(donor_block_function)
            .expect("block donor builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[donor_unit],
                origin(30),
            )
            .expect("unit")[0];
        function
            .terminate(entry, terminator(30, TerminatorKind::Return(unit)))
            .expect("return");
        entry
    };
    donor.finish_checked().expect("valid donor");

    let mut recipient = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let recipient_bool = recipient.type_id(&Type::Bool).expect("recipient Bool type");
    let recipient_unit = recipient.type_id(&Type::Unit).expect("recipient Unit type");
    let type_error = recipient
        .declare_function(
            origin(42),
            "bad.foreign_type",
            Signature::new(Vec::new(), donor_bool),
            Effects::NONE,
        )
        .expect_err("a type ID from another program must be rejected");
    assert_eq!(type_error.code(), BuildErrorCode::InvalidValueType);

    let value_user = recipient
        .declare_function(
            origin(31),
            "bad.foreign_value",
            Signature::new(Vec::new(), recipient_bool),
            Effects::NONE,
        )
        .expect("declare value user");
    let block_user = recipient
        .declare_function(
            origin(32),
            "bad.foreign_block",
            Signature::new(Vec::new(), recipient_unit),
            Effects::NONE,
        )
        .expect("declare block user");
    let call_user = recipient
        .declare_function(
            origin(33),
            "bad.foreign_callee",
            Signature::new(vec![recipient_bool], recipient_bool),
            Effects::NONE,
        )
        .expect("declare call user");
    {
        let mut function = recipient.function(value_user).expect("value user builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::BoolNot { value: donor_value },
                &[recipient_bool],
                origin(31),
            )
            .expect("not")[0];
        function
            .terminate(entry, terminator(31, TerminatorKind::Return(result)))
            .expect("return");
    }
    {
        let mut function = recipient.function(block_user).expect("block user builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        function
            .terminate(
                entry,
                terminator(
                    32,
                    TerminatorKind::Jump(BlockTarget::new(donor_block, Vec::new())),
                ),
            )
            .expect("jump");
    }
    {
        let mut function = recipient.function(call_user).expect("call user builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let argument = function
            .append_block_parameter(entry, recipient_bool)
            .expect("argument");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: donor_function,
                    arguments: vec![argument].into_boxed_slice(),
                },
                &[recipient_bool],
                origin(33),
            )
            .expect("call")[0];
        function
            .terminate(entry, terminator(33, TerminatorKind::Return(result)))
            .expect("return");
    }
    let errors = recipient
        .finish_checked()
        .expect_err("cross-program identities must fail validation");
    for expected in [
        ValidationCode::InvalidValueReference,
        ValidationCode::InvalidBlockReference,
        ValidationCode::InvalidFunctionReference,
    ] {
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code() == expected),
            "missing {expected:?}: {:#?}",
            errors.as_slice()
        );
    }
}

#[test]
fn fault_effect_is_explicit_and_has_a_stable_dump() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(34),
            "example.contract_fault",
            Signature::new(Vec::new(), unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        function
            .terminate(
                entry,
                terminator(
                    34,
                    TerminatorKind::Fault {
                        code: FaultCode::ContractFailed,
                    },
                ),
            )
            .expect("fault");
    }
    let checked = program.finish_checked().expect("valid faulting function");
    let dump = dump_program(&checked);
    assert!(dump.contains("effects=may_fault"));
    assert!(dump.contains("fault ContractFailed"));

    assert_has_code(
        fault_without_effect_program(),
        ValidationCode::EffectMismatch,
    );
}

fn unit_identity_dump(source: u32) -> (loom_codegen_ir::InstanceId, String) {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(source),
            "example.unit_identity",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut builder = program.function(function).expect("builder");
        let entry = builder.create_block().expect("entry");
        builder.set_entry(entry).expect("set entry");
        let unit = builder
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(source),
            )
            .expect("unit")[0];
        builder
            .terminate(entry, terminator(source, TerminatorKind::Return(unit)))
            .expect("return");
    }
    let checked = program.finish_checked().expect("valid unit function");
    (function, dump_program(&checked))
}

fn assert_has_code(program: loom_codegen_ir::Program, expected: ValidationCode) {
    let errors = program
        .into_checked()
        .expect_err("malformed LCIR must fail");
    assert!(
        errors
            .as_slice()
            .iter()
            .any(|error| error.code() == expected),
        "missing {expected:?}: {:#?}",
        errors.as_slice()
    );
}

fn wrong_return_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let function = program
        .declare_function(
            origin(10),
            "bad.return",
            Signature::new(Vec::new(), int_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[bool_ty],
                origin(10),
            )
            .expect("constant")[0];
        function
            .terminate(entry, terminator(10, TerminatorKind::Return(value)))
            .expect("return");
    }
    program.finish()
}

fn bad_edge_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(11),
            "bad.edge",
            Signature::new(vec![int_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let exit = function.create_block().expect("exit");
        function.set_entry(entry).expect("set entry");
        let _argument = function
            .append_block_parameter(entry, int_ty)
            .expect("argument");
        let result = function
            .append_block_parameter(exit, int_ty)
            .expect("result");
        function
            .terminate(
                entry,
                terminator(11, TerminatorKind::Jump(BlockTarget::new(exit, Vec::new()))),
            )
            .expect("jump");
        function
            .terminate(exit, terminator(11, TerminatorKind::Return(result)))
            .expect("return");
    }
    program.finish()
}

fn non_dominating_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(12),
            "bad.dominance",
            Signature::new(vec![bool_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let then_block = function.create_block().expect("then");
        let else_block = function.create_block().expect("else");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_block_parameter(entry, bool_ty)
            .expect("condition");
        let then_value = function
            .append_instruction(
                then_block,
                InstructionKind::Constant(Constant::Int(1)),
                &[int_ty],
                origin(12),
            )
            .expect("constant")[0];
        function
            .terminate(
                entry,
                terminator(
                    12,
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(then_block, Vec::new()),
                        else_target: BlockTarget::new(else_block, Vec::new()),
                    },
                ),
            )
            .expect("branch");
        function
            .terminate(
                then_block,
                terminator(12, TerminatorKind::Return(then_value)),
            )
            .expect("then return");
        function
            .terminate(
                else_block,
                terminator(12, TerminatorKind::Return(then_value)),
            )
            .expect("invalid else return");
    }
    program.finish()
}

fn missing_terminator_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(14),
            "bad.terminator",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
    }
    program.finish()
}

fn entry_predecessor_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(15),
            "bad.entry_predecessor",
            Signature::new(vec![int_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let header = function.create_block().expect("header");
        function.set_entry(entry).expect("set entry");
        let argument = function
            .append_block_parameter(entry, int_ty)
            .expect("argument");
        let carried = function
            .append_block_parameter(header, int_ty)
            .expect("carried");
        function
            .terminate(
                entry,
                terminator(
                    15,
                    TerminatorKind::Jump(BlockTarget::new(header, vec![argument])),
                ),
            )
            .expect("entry jump");
        function
            .terminate(
                header,
                terminator(
                    15,
                    TerminatorKind::Jump(BlockTarget::new(entry, vec![carried])),
                ),
            )
            .expect("back edge");
    }
    program.finish()
}

fn same_target_branch_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let function = program
        .declare_function(
            origin(16),
            "branch.same_target",
            Signature::new(vec![bool_ty, int_ty, int_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let exit = function.create_block().expect("exit");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_block_parameter(entry, bool_ty)
            .expect("condition");
        let first = function
            .append_block_parameter(entry, int_ty)
            .expect("first");
        let second = function
            .append_block_parameter(entry, int_ty)
            .expect("second");
        let result = function
            .append_block_parameter(exit, int_ty)
            .expect("result");
        function
            .terminate(
                entry,
                terminator(
                    16,
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(exit, vec![first]),
                        else_target: BlockTarget::new(exit, vec![second]),
                    },
                ),
            )
            .expect("branch");
        function
            .terminate(exit, terminator(16, TerminatorKind::Return(result)))
            .expect("return");
    }
    program.finish()
}

fn cross_function_value_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let producer = program
        .declare_function(
            origin(17),
            "bad.owner.producer",
            Signature::new(Vec::new(), bool_ty),
            Effects::NONE,
        )
        .expect("declare producer");
    let consumer = program
        .declare_function(
            origin(18),
            "bad.owner.consumer",
            Signature::new(Vec::new(), bool_ty),
            Effects::NONE,
        )
        .expect("declare consumer");
    let foreign_value = {
        let mut function = program.function(producer).expect("producer builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[bool_ty],
                origin(17),
            )
            .expect("constant")[0];
        function
            .terminate(entry, terminator(17, TerminatorKind::Return(value)))
            .expect("return");
        value
    };
    {
        let mut function = program.function(consumer).expect("consumer builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let local = function
            .append_instruction(
                entry,
                InstructionKind::BoolNot {
                    value: foreign_value,
                },
                &[bool_ty],
                origin(18),
            )
            .expect("not")[0];
        function
            .terminate(entry, terminator(18, TerminatorKind::Return(local)))
            .expect("return");
    }
    program.finish()
}

fn uninhabited_call_result_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let never_ty = program.type_id(&Type::Never).expect("Never type");
    let function = program
        .declare_function(
            origin(19),
            "bad.never_value",
            Signature::new(Vec::new(), never_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function_builder = program.function(function).expect("builder");
        let entry = function_builder.create_block().expect("entry");
        function_builder.set_entry(entry).expect("set entry");
        let result = function_builder
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: function,
                    arguments: Vec::new().into_boxed_slice(),
                },
                &[never_ty],
                origin(19),
            )
            .expect("call")[0];
        function_builder
            .terminate(entry, terminator(19, TerminatorKind::Return(result)))
            .expect("return");
    }
    program.finish()
}

fn cross_function_block_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let producer = program
        .declare_function(
            origin(21),
            "bad.block_owner.producer",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare producer");
    let consumer = program
        .declare_function(
            origin(22),
            "bad.block_owner.consumer",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare consumer");
    let foreign_entry = {
        let mut function = program.function(producer).expect("producer builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(21),
            )
            .expect("unit")[0];
        function
            .terminate(entry, terminator(21, TerminatorKind::Return(unit)))
            .expect("return");
        entry
    };
    {
        let mut function = program.function(consumer).expect("consumer builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        function
            .terminate(
                entry,
                terminator(
                    22,
                    TerminatorKind::Jump(BlockTarget::new(foreign_entry, Vec::new())),
                ),
            )
            .expect("foreign jump");
    }
    program.finish()
}

fn origin_mismatch_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(23),
            "bad.origin",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
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
                origin(24),
            )
            .expect("unit")[0];
        function
            .terminate(entry, terminator(23, TerminatorKind::Return(unit)))
            .expect("return");
    }
    program.finish()
}

fn fault_without_effect_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(35),
            "bad.fault_effect",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        function
            .terminate(
                entry,
                terminator(
                    35,
                    TerminatorKind::Fault {
                        code: FaultCode::AssertionFailed,
                    },
                ),
            )
            .expect("fault");
    }
    program.finish()
}

fn missing_entry_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(36),
            "bad.missing_entry",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let block = function.create_block().expect("block");
        let unit = function
            .append_instruction(
                block,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(36),
            )
            .expect("unit")[0];
        function
            .terminate(block, terminator(36, TerminatorKind::Return(unit)))
            .expect("return");
    }
    program.finish()
}

fn entry_signature_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let function = program
        .declare_function(
            origin(37),
            "bad.entry_signature",
            Signature::new(vec![bool_ty], bool_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[bool_ty],
                origin(37),
            )
            .expect("bool")[0];
        function
            .terminate(entry, terminator(37, TerminatorKind::Return(result)))
            .expect("return");
    }
    program.finish()
}

fn unreachable_block_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let function = program
        .declare_function(
            origin(38),
            "bad.unreachable",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        let dead = function.create_block().expect("dead");
        function.set_entry(entry).expect("set entry");
        for block in [entry, dead] {
            let unit = function
                .append_instruction(
                    block,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit_ty],
                    origin(38),
                )
                .expect("unit")[0];
            function
                .terminate(block, terminator(38, TerminatorKind::Return(unit)))
                .expect("return");
        }
    }
    program.finish()
}

fn instruction_result_shape_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let function = program
        .declare_function(
            origin(39),
            "bad.instruction_shape",
            Signature::new(Vec::new(), bool_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare");
    {
        let mut function = program.function(function).expect("builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[],
                origin(39),
            )
            .expect("malformed constant");
        function
            .terminate(
                entry,
                terminator(
                    39,
                    TerminatorKind::Fault {
                        code: FaultCode::AssertionFailed,
                    },
                ),
            )
            .expect("fault");
    }
    program.finish()
}

fn invalid_call_program() -> loom_codegen_ir::Program {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let integer_identity = program
        .declare_function(
            origin(40),
            "bad.call.integer_identity",
            Signature::new(vec![int_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare callee");
    let bad_caller = program
        .declare_function(
            origin(41),
            "bad.call.caller",
            Signature::new(vec![bool_ty], bool_ty),
            Effects::NONE,
        )
        .expect("declare caller");
    {
        let mut function = program.function(integer_identity).expect("callee builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, int_ty)
            .expect("value");
        function
            .terminate(entry, terminator(40, TerminatorKind::Return(value)))
            .expect("return");
    }
    {
        let mut function = program.function(bad_caller).expect("caller builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let argument = function
            .append_block_parameter(entry, bool_ty)
            .expect("argument");
        let result = function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: integer_identity,
                    arguments: vec![argument].into_boxed_slice(),
                },
                &[bool_ty],
                origin(41),
            )
            .expect("call")[0];
        function
            .terminate(entry, terminator(41, TerminatorKind::Return(result)))
            .expect("return");
    }
    program.finish()
}
