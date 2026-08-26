#![allow(clippy::default_trait_access, clippy::too_many_lines)]

use std::process::{Command, Output};

use loom_codegen_ir::{
    ArtifactRootRequest, BlockTarget, BoolPredicate, CheckedArtifact, CheckedIntBinaryOp, Constant,
    Effects, FaultCode, FloatBinaryOp, FloatPredicate, InstructionKind, IntPredicate, Origin,
    ProgramBuilder, ResultTarget, Signature, TargetLayout, Terminator, TerminatorKind,
    UnwindTarget,
};
use loom_codegen_llvm::{
    DebugSource, NativeObjectOptions, OptimizationProfile, emit_lcir_native_object,
    link_native_object,
};
use loom_mir::{FunctionId as MirFunctionId, Type};

fn origin(function: u32) -> Origin {
    Origin::synthetic(MirFunctionId(function))
}

fn terminator(function: u32, kind: TerminatorKind) -> Terminator {
    Terminator::new(kind, origin(function))
}

fn emit_ir(artifact: &CheckedArtifact, directory: &tempfile::TempDir, stem: &str) -> String {
    let object = directory.path().join(format!("{stem}.o"));
    let ir = directory.path().join(format!("{stem}.ll"));
    let options = NativeObjectOptions {
        emit_ir: Some(ir.clone()),
        ..NativeObjectOptions::default()
    };
    let emitted = emit_lcir_native_object(artifact, &object, &options).expect("emit LCIR object");
    assert_eq!(emitted.object, object);
    assert_eq!(emitted.functions, artifact.functions().len());
    assert_eq!(emitted.witnesses, 0);
    assert!(object.is_file());
    std::fs::read_to_string(ir).expect("read emitted LCIR LLVM IR")
}

fn emit_and_run(
    artifact: &CheckedArtifact,
    directory: &tempfile::TempDir,
    stem: &str,
) -> (String, Output) {
    let object = directory.path().join(format!("{stem}.o"));
    let ir = directory.path().join(format!("{stem}.ll"));
    let executable = directory.path().join(stem);
    let options = NativeObjectOptions {
        emit_ir: Some(ir.clone()),
        ..NativeObjectOptions::default()
    };
    emit_lcir_native_object(artifact, &object, &options).expect("emit LCIR object");
    link_native_object(&object, &executable).expect("link LCIR executable");
    let output = Command::new(executable)
        .output()
        .expect("run LCIR executable");
    (
        std::fs::read_to_string(ir).expect("read emitted LCIR LLVM IR"),
        output,
    )
}

fn unit_run(pointer_bits: u16) -> CheckedArtifact {
    let mut program = ProgramBuilder::new(TargetLayout::new(pointer_bits).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let root = program
        .declare_function(
            origin(1),
            "main",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare root");
    {
        let mut function = program.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(1),
            )
            .expect("unit")[0];
        function
            .terminate(entry, terminator(1, TerminatorKind::Return(unit)))
            .expect("return");
    }
    program
        .finish_checked()
        .expect("checked unit LCIR")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("unit run artifact")
}

fn assert_no_legacy_ir(ir: &str) {
    for forbidden in [
        "loom.Value",
        "ArgNode",
        "ValueNode",
        "loom.runtime.print",
        "loom_executor_",
        "loom_gc_",
        "witness",
        "landingpad",
        "personality ptr",
        "resume {",
    ] {
        assert!(
            !ir.contains(forbidden),
            "legacy/EH token `{forbidden}` in:\n{ir}"
        );
    }
}

#[test]
fn pure_run_has_the_zst_abi_and_no_runtime_or_legacy_surface() {
    let artifact = unit_run(64);
    let directory = tempfile::tempdir().expect("temp directory");
    let (ir, output) = emit_and_run(&artifact, &directory, "pure-unit");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
    assert!(ir.contains("define internal {} @loom.lcir.fn.0()"), "{ir}");
    assert!(!ir.contains("loom_runtime_"), "{ir}");
    assert!(!ir.contains("loom_context_raise_fault_v1"), "{ir}");
    assert_no_legacy_ir(&ir);
}

#[test]
fn cfg_preorder_not_block_insertion_order_drives_llvm_emission() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let root = program
        .declare_function(
            origin(2),
            "reverse.block.order",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare root");
    {
        let mut function = program.function(root).expect("root builder");
        // Deliberately create the exit and its dominator before creating the
        // entry. Checked LCIR constrains these blocks by CFG dominance, not by
        // their dense insertion order.
        let exit = function.create_block().expect("exit");
        let body = function.create_block().expect("body");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let unit = function
            .append_instruction(
                body,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(2),
            )
            .expect("unit")[0];
        function
            .terminate(
                entry,
                terminator(2, TerminatorKind::Jump(BlockTarget::new(body, Vec::new()))),
            )
            .expect("enter body");
        function
            .terminate(
                body,
                terminator(2, TerminatorKind::Jump(BlockTarget::new(exit, Vec::new()))),
            )
            .expect("enter exit");
        function
            .terminate(exit, terminator(2, TerminatorKind::Return(unit)))
            .expect("return dominated value");
    }
    let artifact = program
        .finish_checked()
        .expect("checked reverse-order CFG")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("reverse-order artifact");
    let directory = tempfile::tempdir().expect("temp directory");
    let (ir, output) = emit_and_run(&artifact, &directory, "reverse-block-order");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
    let entry = ir.find("b2:").expect("LCIR entry block in LLVM IR");
    let body = ir.find("b1:").expect("LCIR body block in LLVM IR");
    let exit = ir.find("b0:").expect("LCIR exit block in LLVM IR");
    assert!(
        entry < body && body < exit,
        "LLVM CFG is not in preorder:\n{ir}"
    );
    assert_no_legacy_ir(&ir);
}

#[test]
fn scalar_abis_direct_calls_phi_predicates_and_float_bits_are_mechanical() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let float_ty = program.type_id(&Type::Float).expect("Float type");
    let bool_identity = program
        .declare_function(
            origin(10),
            "bool.identity",
            Signature::new(vec![bool_ty], bool_ty),
            Effects::NONE,
        )
        .expect("bool declaration");
    let select_int = program
        .declare_function(
            origin(11),
            "int.select",
            Signature::new(vec![bool_ty, int_ty, int_ty], int_ty),
            Effects::NONE,
        )
        .expect("int declaration");
    let float_identity = program
        .declare_function(
            origin(12),
            "float.identity",
            Signature::new(vec![float_ty], float_ty),
            Effects::NONE,
        )
        .expect("float declaration");
    let unit_identity = program
        .declare_function(
            origin(13),
            "unit.identity",
            Signature::new(vec![unit_ty], unit_ty),
            Effects::NONE,
        )
        .expect("unit declaration");
    let root = program
        .declare_function(
            origin(14),
            "main",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("root declaration");

    {
        let mut function = program.function(bool_identity).expect("bool builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, bool_ty)
            .expect("value");
        function
            .append_instruction(
                entry,
                InstructionKind::BoolNot { value },
                &[bool_ty],
                origin(10),
            )
            .expect("not");
        function
            .terminate(entry, terminator(10, TerminatorKind::Return(value)))
            .expect("return");
    }
    {
        let mut function = program.function(select_int).expect("select builder");
        let entry = function.create_block().expect("entry");
        let then_block = function.create_block().expect("then");
        let else_block = function.create_block().expect("else");
        let join = function.create_block().expect("join");
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
        let then_value = function
            .append_block_parameter(then_block, int_ty)
            .expect("then value");
        let else_value = function
            .append_block_parameter(else_block, int_ty)
            .expect("else value");
        let selected = function
            .append_block_parameter(join, int_ty)
            .expect("selected");
        function
            .terminate(
                entry,
                terminator(
                    11,
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(then_block, vec![left]),
                        else_target: BlockTarget::new(else_block, vec![right]),
                    },
                ),
            )
            .expect("branch");
        function
            .terminate(
                then_block,
                terminator(
                    11,
                    TerminatorKind::Jump(BlockTarget::new(join, vec![then_value])),
                ),
            )
            .expect("then jump");
        function
            .terminate(
                else_block,
                terminator(
                    11,
                    TerminatorKind::Jump(BlockTarget::new(join, vec![else_value])),
                ),
            )
            .expect("else jump");
        function
            .terminate(join, terminator(11, TerminatorKind::Return(selected)))
            .expect("return");
    }
    {
        let mut function = program.function(float_identity).expect("float builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, float_ty)
            .expect("value");
        function
            .terminate(entry, terminator(12, TerminatorKind::Return(value)))
            .expect("return");
    }
    {
        let mut function = program.function(unit_identity).expect("unit builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let value = function
            .append_block_parameter(entry, unit_ty)
            .expect("value");
        function
            .terminate(entry, terminator(13, TerminatorKind::Return(value)))
            .expect("return");
    }
    {
        let mut function = program.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[bool_ty],
                origin(14),
            )
            .expect("condition")[0];
        let left = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(7)),
                &[int_ty],
                origin(14),
            )
            .expect("left")[0];
        let right = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(9)),
                &[int_ty],
                origin(14),
            )
            .expect("right")[0];
        let nan_bits = 0x7ff8_0000_0000_00a5;
        let float = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::FloatBits(nan_bits)),
                &[float_ty],
                origin(14),
            )
            .expect("float")[0];
        for (callee, arguments, result_ty) in [
            (bool_identity, vec![condition], bool_ty),
            (select_int, vec![condition, left, right], int_ty),
            (float_identity, vec![float], float_ty),
        ] {
            function
                .append_instruction(
                    entry,
                    InstructionKind::DirectCall {
                        callee,
                        arguments: arguments.into_boxed_slice(),
                    },
                    &[result_ty],
                    origin(14),
                )
                .expect("direct call");
        }
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(14),
            )
            .expect("unit")[0];
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: unit_identity,
                    arguments: vec![unit].into_boxed_slice(),
                },
                &[unit_ty],
                origin(14),
            )
            .expect("unit direct call")[0];
        function
            .terminate(entry, terminator(14, TerminatorKind::Return(unit)))
            .expect("return");
    }

    let artifact = program
        .finish_checked()
        .expect("checked typed LCIR")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("closed typed artifact");
    let directory = tempfile::tempdir().expect("temp directory");
    let ir = emit_ir(&artifact, &directory, "scalar-abi");
    for signature in [
        "define internal i1 @loom.lcir.fn.0(i1",
        "define internal i64 @loom.lcir.fn.1(i1",
        "define internal double @loom.lcir.fn.2(double",
        "define internal {} @loom.lcir.fn.3({}",
        "define internal {} @loom.lcir.fn.4()",
    ] {
        assert!(ir.contains(signature), "missing `{signature}` in:\n{ir}");
    }
    assert!(ir.contains("phi i64"), "{ir}");
    assert!(!ir.contains("branch.then.edge"), "{ir}");
    assert!(!ir.contains("branch.else.edge"), "{ir}");
    assert!(ir.contains("call i1 @loom.lcir.fn.0"), "{ir}");
    assert!(ir.contains("0x7FF80000000000A5"), "{ir}");
    assert_no_legacy_ir(&ir);
}

#[test]
fn same_target_branch_edges_keep_distinct_phi_arguments_via_minimal_normalization() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let choose = program
        .declare_function(
            origin(16),
            "same.target.choose",
            Signature::new(vec![bool_ty, int_ty, int_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare choose");
    let root = program
        .declare_function(
            origin(17),
            "main",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare root");
    {
        let mut function = program.function(choose).expect("choose builder");
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
        let selected = function
            .append_block_parameter(exit, int_ty)
            .expect("selected");
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
            .expect("same-target branch");
        function
            .terminate(exit, terminator(16, TerminatorKind::Return(selected)))
            .expect("return");
    }
    {
        let mut function = program.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(true)),
                &[bool_ty],
                origin(17),
            )
            .expect("condition")[0];
        let first = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(11)),
                &[int_ty],
                origin(17),
            )
            .expect("first")[0];
        let second = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(22)),
                &[int_ty],
                origin(17),
            )
            .expect("second")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: choose,
                    arguments: vec![condition, first, second].into_boxed_slice(),
                },
                &[int_ty],
                origin(17),
            )
            .expect("choose call");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(17),
            )
            .expect("unit")[0];
        function
            .terminate(entry, terminator(17, TerminatorKind::Return(unit)))
            .expect("return");
    }
    let artifact = program
        .finish_checked()
        .expect("checked same-target branch")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("same-target artifact");
    let directory = tempfile::tempdir().expect("temp directory");
    let ir = emit_ir(&artifact, &directory, "same-target-branch");
    assert!(ir.contains("branch.then.edge:"), "{ir}");
    assert!(ir.contains("branch.else.edge:"), "{ir}");
    assert!(
        ir.contains("[ %1, %branch.then.edge ], [ %2, %branch.else.edge ]"),
        "same-target values did not retain separate predecessors:\n{ir}"
    );
}

#[test]
fn identical_same_target_branch_arms_collapse_to_one_direct_edge() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let choose = program
        .declare_function(
            origin(17),
            "same.target.identical",
            Signature::new(vec![bool_ty, int_ty], int_ty),
            Effects::NONE,
        )
        .expect("declare choose");
    let root = program
        .declare_function(
            origin(18),
            "main",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare root");
    {
        let mut function = program.function(choose).expect("choose builder");
        let entry = function.create_block().expect("entry");
        let exit = function.create_block().expect("exit");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_block_parameter(entry, bool_ty)
            .expect("condition");
        let value = function
            .append_block_parameter(entry, int_ty)
            .expect("value");
        let selected = function
            .append_block_parameter(exit, int_ty)
            .expect("selected");
        let target = BlockTarget::new(exit, vec![value]);
        function
            .terminate(
                entry,
                terminator(
                    17,
                    TerminatorKind::Branch {
                        condition,
                        then_target: target.clone(),
                        else_target: target,
                    },
                ),
            )
            .expect("identical branch");
        function
            .terminate(exit, terminator(17, TerminatorKind::Return(selected)))
            .expect("return");
    }
    {
        let mut function = program.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(false)),
                &[bool_ty],
                origin(18),
            )
            .expect("condition")[0];
        let value = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(7)),
                &[int_ty],
                origin(18),
            )
            .expect("value")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: choose,
                    arguments: vec![condition, value].into_boxed_slice(),
                },
                &[int_ty],
                origin(18),
            )
            .expect("choose call");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(18),
            )
            .expect("unit")[0];
        function
            .terminate(entry, terminator(18, TerminatorKind::Return(unit)))
            .expect("return");
    }
    let artifact = program
        .finish_checked()
        .expect("checked identical branch")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("identical-branch artifact");
    let directory = tempfile::tempdir().expect("temp directory");
    let ir = emit_ir(&artifact, &directory, "identical-same-target");

    assert!(!ir.contains("branch.then.edge"), "{ir}");
    assert!(!ir.contains("branch.else.edge"), "{ir}");
    assert!(
        !ir.contains("br i1"),
        "identical arms kept a conditional:\n{ir}"
    );
    assert!(ir.contains("br label %b1"), "missing direct edge:\n{ir}");
}

#[test]
fn loop_header_phi_accepts_a_backedge_without_synthetic_branch_blocks() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let float_ty = program.type_id(&Type::Float).expect("Float type");
    let looping = program
        .declare_function(
            origin(18),
            "loop.header",
            Signature::new(vec![bool_ty, float_ty], float_ty),
            Effects::NONE,
        )
        .expect("declare loop");
    let root = program
        .declare_function(
            origin(19),
            "main",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare root");
    {
        let mut function = program.function(looping).expect("loop builder");
        let entry = function.create_block().expect("entry");
        let header = function.create_block().expect("header");
        let body = function.create_block().expect("body");
        let exit = function.create_block().expect("exit");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_block_parameter(entry, bool_ty)
            .expect("condition");
        let start = function
            .append_block_parameter(entry, float_ty)
            .expect("start");
        let current = function
            .append_block_parameter(header, float_ty)
            .expect("current");
        let result = function
            .append_block_parameter(exit, float_ty)
            .expect("result");
        function
            .terminate(
                entry,
                terminator(
                    18,
                    TerminatorKind::Jump(BlockTarget::new(header, vec![start])),
                ),
            )
            .expect("enter loop");
        let next_value = function
            .append_instruction(
                body,
                InstructionKind::FloatNegate { value: current },
                &[float_ty],
                origin(18),
            )
            .expect("backedge value")[0];
        function
            .terminate(
                header,
                terminator(
                    18,
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(body, Vec::new()),
                        else_target: BlockTarget::new(exit, vec![current]),
                    },
                ),
            )
            .expect("loop branch");
        function
            .terminate(
                body,
                terminator(
                    18,
                    TerminatorKind::Jump(BlockTarget::new(header, vec![next_value])),
                ),
            )
            .expect("backedge");
        function
            .terminate(exit, terminator(18, TerminatorKind::Return(result)))
            .expect("return");
    }
    {
        let mut function = program.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(false)),
                &[bool_ty],
                origin(19),
            )
            .expect("condition")[0];
        let zero = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::float(0.0)),
                &[float_ty],
                origin(19),
            )
            .expect("zero")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: looping,
                    arguments: vec![condition, zero].into_boxed_slice(),
                },
                &[float_ty],
                origin(19),
            )
            .expect("loop call");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(19),
            )
            .expect("unit")[0];
        function
            .terminate(entry, terminator(19, TerminatorKind::Return(unit)))
            .expect("return");
    }
    let artifact = program
        .finish_checked()
        .expect("checked loop")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("loop artifact");
    let directory = tempfile::tempdir().expect("temp directory");
    let (ir, output) = emit_and_run(&artifact, &directory, "loop-phi");
    assert!(output.status.success(), "{output:?}");
    assert!(
        ir.contains("phi double [ %1, %b0 ], [ %float.negate, %b2 ]"),
        "{ir}"
    );
    assert!(!ir.contains("branch.then.edge"), "{ir}");
    assert!(!ir.contains("branch.else.edge"), "{ir}");
}

#[test]
fn every_current_pure_scalar_opcode_has_the_exact_llvm_predicate() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let float_ty = program.type_id(&Type::Float).expect("Float type");
    let operations = program
        .declare_function(
            origin(14),
            "scalar.operations",
            Signature::new(
                vec![bool_ty, bool_ty, int_ty, int_ty, float_ty, float_ty],
                unit_ty,
            ),
            Effects::NONE,
        )
        .expect("declare operations");
    let root = program
        .declare_function(
            origin(15),
            "main",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare root");
    {
        let mut function = program.function(operations).expect("operations builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let first_bool = function
            .append_block_parameter(entry, bool_ty)
            .expect("first bool");
        let second_bool = function
            .append_block_parameter(entry, bool_ty)
            .expect("second bool");
        let first_int = function
            .append_block_parameter(entry, int_ty)
            .expect("first int");
        let second_int = function
            .append_block_parameter(entry, int_ty)
            .expect("second int");
        let first_float = function
            .append_block_parameter(entry, float_ty)
            .expect("first float");
        let second_float = function
            .append_block_parameter(entry, float_ty)
            .expect("second float");
        function
            .append_instruction(
                entry,
                InstructionKind::BoolNot { value: first_bool },
                &[bool_ty],
                origin(14),
            )
            .expect("bool not");
        for predicate in [BoolPredicate::Equal, BoolPredicate::NotEqual] {
            function
                .append_instruction(
                    entry,
                    InstructionKind::BoolCompare {
                        predicate,
                        left: first_bool,
                        right: second_bool,
                    },
                    &[bool_ty],
                    origin(14),
                )
                .expect("bool compare");
        }
        for predicate in [
            IntPredicate::Equal,
            IntPredicate::NotEqual,
            IntPredicate::Less,
            IntPredicate::LessEqual,
            IntPredicate::Greater,
            IntPredicate::GreaterEqual,
        ] {
            function
                .append_instruction(
                    entry,
                    InstructionKind::IntCompare {
                        predicate,
                        left: first_int,
                        right: second_int,
                    },
                    &[bool_ty],
                    origin(14),
                )
                .expect("int compare");
        }
        function
            .append_instruction(
                entry,
                InstructionKind::FloatNegate { value: first_float },
                &[float_ty],
                origin(14),
            )
            .expect("float negate");
        for op in [
            FloatBinaryOp::Add,
            FloatBinaryOp::Subtract,
            FloatBinaryOp::Multiply,
            FloatBinaryOp::Divide,
        ] {
            function
                .append_instruction(
                    entry,
                    InstructionKind::FloatBinary {
                        op,
                        left: first_float,
                        right: second_float,
                    },
                    &[float_ty],
                    origin(14),
                )
                .expect("float binary");
        }
        for predicate in [
            FloatPredicate::OrderedEqual,
            FloatPredicate::UnorderedNotEqual,
            FloatPredicate::OrderedLess,
            FloatPredicate::OrderedLessEqual,
            FloatPredicate::OrderedGreater,
            FloatPredicate::OrderedGreaterEqual,
        ] {
            function
                .append_instruction(
                    entry,
                    InstructionKind::FloatCompare {
                        predicate,
                        left: first_float,
                        right: second_float,
                    },
                    &[bool_ty],
                    origin(14),
                )
                .expect("float compare");
        }
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(14),
            )
            .expect("unit")[0];
        function
            .terminate(entry, terminator(14, TerminatorKind::Return(unit)))
            .expect("return");
    }
    {
        let mut function = program.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let mut arguments = Vec::new();
        for (constant, ty) in [
            (Constant::Bool(true), bool_ty),
            (Constant::Bool(false), bool_ty),
            (Constant::Int(2), int_ty),
            (Constant::Int(3), int_ty),
            (Constant::float(1.5), float_ty),
            (Constant::float(2.5), float_ty),
        ] {
            arguments.push(
                function
                    .append_instruction(
                        entry,
                        InstructionKind::Constant(constant),
                        &[ty],
                        origin(15),
                    )
                    .expect("argument")[0],
            );
        }
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::DirectCall {
                    callee: operations,
                    arguments: arguments.into_boxed_slice(),
                },
                &[unit_ty],
                origin(15),
            )
            .expect("operations call")[0];
        function
            .terminate(entry, terminator(15, TerminatorKind::Return(unit)))
            .expect("return");
    }
    let artifact = program
        .finish_checked()
        .expect("checked scalar operations")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("scalar operations artifact");
    let directory = tempfile::tempdir().expect("temp directory");
    let ir = emit_ir(&artifact, &directory, "scalar-operations");
    for opcode in [
        "xor i1 %0, true",
        "icmp eq i1 %0, %1",
        "icmp ne i1 %0, %1",
        "icmp eq i64 %2, %3",
        "icmp ne i64 %2, %3",
        "icmp slt i64 %2, %3",
        "icmp sle i64 %2, %3",
        "icmp sgt i64 %2, %3",
        "icmp sge i64 %2, %3",
        "fneg double %4",
        "fadd double %4, %5",
        "fsub double %4, %5",
        "fmul double %4, %5",
        "fdiv double %4, %5",
        "fcmp oeq double %4, %5",
        "fcmp une double %4, %5",
        "fcmp olt double %4, %5",
        "fcmp ole double %4, %5",
        "fcmp ogt double %4, %5",
        "fcmp oge double %4, %5",
    ] {
        assert!(ir.contains(opcode), "missing `{opcode}` in:\n{ir}");
    }
    for fast_math in ["fadd fast", "fsub fast", "fmul fast", "fdiv fast"] {
        assert!(
            !ir.contains(fast_math),
            "unexpected `{fast_math}` in:\n{ir}"
        );
    }
}

#[test]
fn checked_integer_operations_use_intrinsics_and_guard_signed_division() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let divide = program
        .declare_function(
            origin(19),
            "checked.divide",
            Signature::new(vec![int_ty, int_ty], int_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare divide");
    let root = program
        .declare_function(
            origin(20),
            "checked.integer.root",
            Signature::new(Vec::new(), unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare root");
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
                    19,
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
            .terminate(normal, terminator(19, TerminatorKind::Return(result)))
            .expect("return");
        function
            .terminate(fault, terminator(19, TerminatorKind::ResumeFault))
            .expect("resume");
    }
    {
        let mut function = program.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        let after_add = function.create_block().expect("after add");
        let after_subtract = function.create_block().expect("after subtract");
        let after_multiply = function.create_block().expect("after multiply");
        let after_negate = function.create_block().expect("after negate");
        let after_divide = function.create_block().expect("after divide");
        let fault = function.create_block().expect("fault");
        function.set_entry(entry).expect("set entry");
        let one = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(1)),
                &[int_ty],
                origin(20),
            )
            .expect("one")[0];
        let two = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(2)),
                &[int_ty],
                origin(20),
            )
            .expect("two")[0];
        let added = function
            .append_block_parameter(after_add, int_ty)
            .expect("added");
        let subtracted = function
            .append_block_parameter(after_subtract, int_ty)
            .expect("subtracted");
        let multiplied = function
            .append_block_parameter(after_multiply, int_ty)
            .expect("multiplied");
        let negated = function
            .append_block_parameter(after_negate, int_ty)
            .expect("negated");
        function
            .append_block_parameter(after_divide, int_ty)
            .expect("divided");
        for (block, op, left, normal) in [
            (entry, CheckedIntBinaryOp::Add, one, after_add),
            (
                after_add,
                CheckedIntBinaryOp::Subtract,
                added,
                after_subtract,
            ),
            (
                after_subtract,
                CheckedIntBinaryOp::Multiply,
                subtracted,
                after_multiply,
            ),
        ] {
            function
                .terminate(
                    block,
                    terminator(
                        20,
                        TerminatorKind::CheckedIntBinary {
                            op,
                            left,
                            right: two,
                            normal: ResultTarget::new(normal, Vec::new()),
                            fault: UnwindTarget::new(fault, Vec::new()),
                        },
                    ),
                )
                .expect("checked binary");
        }
        function
            .terminate(
                after_multiply,
                terminator(
                    20,
                    TerminatorKind::CheckedIntNegate {
                        value: multiplied,
                        normal: ResultTarget::new(after_negate, Vec::new()),
                        fault: UnwindTarget::new(fault, Vec::new()),
                    },
                ),
            )
            .expect("checked negate");
        function
            .terminate(
                after_negate,
                terminator(
                    20,
                    TerminatorKind::Invoke {
                        callee: divide,
                        arguments: vec![negated, one].into_boxed_slice(),
                        normal: ResultTarget::new(after_divide, Vec::new()),
                        unwind: UnwindTarget::new(fault, Vec::new()),
                    },
                ),
            )
            .expect("invoke checked divide");
        let unit = function
            .append_instruction(
                after_divide,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(20),
            )
            .expect("unit")[0];
        function
            .terminate(after_divide, terminator(20, TerminatorKind::Return(unit)))
            .expect("return");
        function
            .terminate(fault, terminator(20, TerminatorKind::ResumeFault))
            .expect("resume");
    }
    let artifact = program
        .finish_checked()
        .expect("checked integer LCIR")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("checked integer artifact");
    let directory = tempfile::tempdir().expect("temp directory");
    let (ir, output) = emit_and_run(&artifact, &directory, "checked-integer");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
    for intrinsic in [
        "llvm.sadd.with.overflow.i64",
        "llvm.ssub.with.overflow.i64",
        "llvm.smul.with.overflow.i64",
    ] {
        assert!(ir.contains(intrinsic), "missing `{intrinsic}` in:\n{ir}");
    }
    assert!(ir.contains("division.by.zero = icmp eq i64 %1, 0"), "{ir}");
    assert!(ir.contains("division.overflows"), "{ir}");
    assert!(ir.contains("division.is.minus.one"), "{ir}");
    assert!(ir.contains("sdiv i64"), "{ir}");
    assert!(!ir.contains(" sdiv i64 -9223372036854775808, -1"), "{ir}");
    assert!(
        ir.contains("define internal { i32, i64 } @loom.lcir.fn.0(i64 %0, i64 %1, ptr %2)"),
        "{ir}"
    );
    assert!(
        ir.contains("define internal { i32, {} } @loom.lcir.fn.1(ptr %0)"),
        "{ir}"
    );
    assert!(
        ir.contains("%loom.lcir.FaultContext = type { ptr, i1 }"),
        "{ir}"
    );
    let fault_declaration = ir
        .lines()
        .find(|line| line.contains("declare i32 @loom_context_raise_fault_v1"))
        .expect("fault runtime declaration");
    assert!(
        fault_declaration.contains(" #"),
        "{fault_declaration}\n{ir}"
    );
    assert!(ir.contains("cold noinline"), "{ir}");
    assert_no_legacy_ir(&ir);
}

#[test]
fn invoke_shares_one_first_primary_fault_context_across_active_cleanup() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let int_ty = program.type_id(&Type::Int).expect("Int type");
    let leaf = program
        .declare_function(
            origin(30),
            "faulting.leaf",
            Signature::new(Vec::new(), unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare leaf");
    let root = program
        .declare_function(
            origin(31),
            "cleanup.root",
            Signature::new(Vec::new(), unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare root");
    {
        let mut function = program.function(leaf).expect("leaf builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        function
            .terminate(
                entry,
                terminator(
                    30,
                    TerminatorKind::Fault {
                        code: FaultCode::AssertionFailed,
                    },
                ),
            )
            .expect("fault");
    }
    {
        let mut function = program.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        let returned = function.create_block().expect("returned");
        let cleanup = function.create_block().expect("cleanup");
        let cleanup_succeeded = function.create_block().expect("cleanup success");
        let cleanup_faulted = function.create_block().expect("cleanup fault");
        function.set_entry(entry).expect("set entry");
        let minimum = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Int(i64::MIN)),
                &[int_ty],
                origin(31),
            )
            .expect("minimum")[0];
        let result = function
            .append_block_parameter(returned, unit_ty)
            .expect("invoke result");
        let cleanup_value = function
            .append_block_parameter(cleanup, int_ty)
            .expect("cleanup value");
        function
            .append_block_parameter(cleanup_succeeded, int_ty)
            .expect("negated result");
        function
            .terminate(
                entry,
                terminator(
                    31,
                    TerminatorKind::Invoke {
                        callee: leaf,
                        arguments: Vec::new().into_boxed_slice(),
                        normal: ResultTarget::new(returned, Vec::new()),
                        unwind: UnwindTarget::new(cleanup, vec![minimum]),
                    },
                ),
            )
            .expect("invoke");
        function
            .terminate(returned, terminator(31, TerminatorKind::Return(result)))
            .expect("return");
        function
            .terminate(
                cleanup,
                terminator(
                    31,
                    TerminatorKind::CheckedIntNegate {
                        value: cleanup_value,
                        normal: ResultTarget::new(cleanup_succeeded, Vec::new()),
                        fault: UnwindTarget::new(cleanup_faulted, Vec::new()),
                    },
                ),
            )
            .expect("cleanup negate");
        for block in [cleanup_succeeded, cleanup_faulted] {
            function
                .terminate(block, terminator(31, TerminatorKind::ResumeFault))
                .expect("resume primary fault");
        }
    }
    let artifact = program
        .finish_checked()
        .expect("checked invoke LCIR")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("invoke artifact");
    let directory = tempfile::tempdir().expect("temp directory");
    let (ir, output) = emit_and_run(&artifact, &directory, "invoke-primary");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert_eq!(stdout.matches("AssertionFailed").count(), 1, "{stdout}");
    assert!(!stdout.contains("IntegerOverflow"), "{stdout}");
    assert!(
        ir.contains("call { i32, {} } @loom.lcir.fn.0(ptr %0)"),
        "{ir}"
    );
    assert!(ir.contains("invoke.status"), "{ir}");
    assert!(ir.contains("fault.suppress.secondary"), "{ir}");
    assert!(
        !ir.contains(" invoke "),
        "LLVM EH invoke leaked into:\n{ir}"
    );
    assert_no_legacy_ir(&ir);
}

#[test]
fn tests_harness_is_ordered_continues_after_fault_and_never_creates_an_executor() {
    let mut program = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = program.type_id(&Type::Unit).expect("Unit type");
    let bool_ty = program.type_id(&Type::Bool).expect("Bool type");
    let first = program
        .declare_function(
            origin(40),
            "alpha",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare alpha");
    let second = program
        .declare_function(
            origin(41),
            "beta",
            Signature::new(Vec::new(), unit_ty),
            Effects::MAY_FAULT,
        )
        .expect("declare beta");
    let third = program
        .declare_function(
            origin(42),
            "gamma",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare gamma");
    for (id, source) in [(first, 40), (third, 42)] {
        let mut function = program.function(id).expect("pure test builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
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
    {
        let mut function = program.function(second).expect("faulting test builder");
        let entry = function.create_block().expect("entry");
        let success = function.create_block().expect("success");
        let fault = function.create_block().expect("fault");
        function.set_entry(entry).expect("set entry");
        let condition = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Bool(false)),
                &[bool_ty],
                origin(41),
            )
            .expect("condition")[0];
        function
            .terminate(
                entry,
                terminator(
                    41,
                    TerminatorKind::Assert {
                        condition,
                        code: FaultCode::ContractFailed,
                        success: BlockTarget::new(success, Vec::new()),
                        fault: UnwindTarget::new(fault, Vec::new()),
                    },
                ),
            )
            .expect("assert");
        let unit = function
            .append_instruction(
                success,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origin(41),
            )
            .expect("unit")[0];
        function
            .terminate(success, terminator(41, TerminatorKind::Return(unit)))
            .expect("return");
        function
            .terminate(fault, terminator(41, TerminatorKind::ResumeFault))
            .expect("resume");
    }
    let artifact = program
        .finish_checked()
        .expect("checked tests LCIR")
        .into_artifact(ArtifactRootRequest::tests([first, second, third]))
        .expect("tests artifact");
    let directory = tempfile::tempdir().expect("temp directory");
    let (ir, output) = emit_and_run(&artifact, &directory, "mixed-tests");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    let harness_lines = stdout
        .lines()
        .filter(|line| line.starts_with("passed ") || line.starts_with("failed "))
        .collect::<Vec<_>>();
    assert_eq!(
        harness_lines,
        ["passed alpha", "failed beta", "passed gamma"]
    );
    assert_eq!(stdout.matches("ContractFailed").count(), 1, "{stdout}");
    assert!(ir.contains("loom_runtime_create_v1"), "{ir}");
    assert!(!ir.contains("loom_executor_"), "{ir}");
    assert!(ir.contains("define internal {} @loom.lcir.fn.0()"), "{ir}");
    assert!(
        ir.contains("define internal { i32, {} } @loom.lcir.fn.1(ptr"),
        "{ir}"
    );
    assert!(ir.contains("define internal {} @loom.lcir.fn.2()"), "{ir}");
    for setup_failure in [
        "test.runtime.create.failed:",
        "test.runtime.activation.failed:",
        "RuntimeFault: runtime creation failed",
        "RuntimeFault: runtime activation failed",
    ] {
        assert!(
            ir.contains(setup_failure),
            "missing `{setup_failure}`:\n{ir}"
        );
    }
    assert_eq!(
        ir.matches("ret i32 6").count(),
        2,
        "runtime setup failures must terminate the harness:\n{ir}"
    );
    assert!(!ir.contains("test.runtime.setup.failed"), "{ir}");
    let activation_failure = ir
        .find("test.runtime.activation.failed:")
        .expect("activation-failure block");
    let activation_failure = &ir[activation_failure..];
    let destroy = activation_failure
        .find("call i32 @loom_runtime_destroy_v1")
        .expect("activation failure destroys the inactive runtime");
    let diagnostic = activation_failure
        .find("call i32 @puts")
        .expect("activation failure reports a RuntimeFault");
    assert!(
        destroy < diagnostic,
        "activation failure must destroy before reporting:\n{activation_failure}"
    );
    let deactivate = ir
        .find("runtime.root.deactivate")
        .expect("successful test deactivates its runtime");
    let normal_destroy = ir
        .find("runtime.root.destroy")
        .expect("successful test destroys its runtime");
    assert!(
        deactivate < normal_destroy,
        "normal test cleanup must deactivate before destroy:\n{ir}"
    );
    assert_no_legacy_ir(&ir);
}

#[test]
fn empty_tests_return_success_without_runtime_or_output() {
    let artifact = ProgramBuilder::new(TargetLayout::new(64).expect("target"))
        .finish_checked()
        .expect("checked empty LCIR")
        .into_artifact(ArtifactRootRequest::tests(Vec::new()))
        .expect("empty tests artifact");
    let directory = tempfile::tempdir().expect("temp directory");
    let (ir, output) = emit_and_run(&artifact, &directory, "empty-tests");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(!ir.contains("loom_runtime_"), "{ir}");
    assert_no_legacy_ir(&ir);
}

#[test]
fn target_layout_mismatch_is_rejected_before_ir_emission() {
    let artifact = unit_run(32);
    let directory = tempfile::tempdir().expect("temp directory");
    let error = emit_lcir_native_object(
        &artifact,
        &directory.path().join("mismatch.o"),
        &NativeObjectOptions::default(),
    )
    .expect_err("host target must disagree with 32-bit LCIR");
    assert_eq!(error.code(), "LcirTargetLayoutMismatch");
    assert!(error.message().contains("32-bit pointers"), "{error}");
}

#[test]
fn release_pipeline_emits_a_verified_optimized_object() {
    let artifact = unit_run(64);
    let directory = tempfile::tempdir().expect("temp directory");
    let object = directory.path().join("release.o");
    let options = NativeObjectOptions::default().with_optimization(OptimizationProfile::Release);
    emit_lcir_native_object(&artifact, &object, &options).expect("emit release LCIR object");
    assert!(object.is_file());
    assert!(
        std::fs::metadata(object).expect("release metadata").len() > 0,
        "release object must not be empty"
    );
}

#[test]
fn debug_sources_emit_function_abi_and_expression_locations() {
    let artifact = unit_run(64);
    let directory = tempfile::tempdir().expect("temp directory");
    let object = directory.path().join("debug.o");
    let ir_path = directory.path().join("debug.ll");
    let options = NativeObjectOptions {
        emit_ir: Some(ir_path.clone()),
        debug_sources: vec![DebugSource::new(
            0,
            "src/main.loom",
            "fn main() Unit { Unit }\n",
        )],
        ..NativeObjectOptions::default()
    };
    emit_lcir_native_object(&artifact, &object, &options).expect("emit debug LCIR object");
    let ir = std::fs::read_to_string(ir_path).expect("read debug IR");
    assert!(ir.contains("!DICompileUnit"), "{ir}");
    assert!(ir.contains("src/main.loom"), "{ir}");
    assert!(ir.contains("!DISubprogram"), "{ir}");
    assert!(ir.contains("name: \"main\""), "{ir}");
    assert!(ir.contains("!DISubroutineType"), "{ir}");
    assert!(ir.contains("!DILocation"), "{ir}");
}

#[test]
fn debug_sources_fail_closed_on_duplicate_and_missing_file_ids() {
    let artifact = unit_run(64);
    let directory = tempfile::tempdir().expect("temp directory");
    for (name, sources, expected) in [
        (
            "duplicate",
            vec![
                DebugSource::new(0, "src/main.loom", "fn main() Unit { Unit }\n"),
                DebugSource::new(0, "src/alias.loom", "fn alias() Unit { Unit }\n"),
            ],
            "duplicate debug source file id #0",
        ),
        (
            "missing",
            vec![DebugSource::new(
                7,
                "src/unrelated.loom",
                "fn unrelated() Unit { Unit }\n",
            )],
            "debug source table does not contain file id #0",
        ),
    ] {
        let error = emit_lcir_native_object(
            &artifact,
            &directory.path().join(format!("{name}.o")),
            &NativeObjectOptions {
                debug_sources: sources,
                ..NativeObjectOptions::default()
            },
        )
        .expect_err("invalid debug source identities must fail closed");
        assert_eq!(error.code(), "LlvmDebugInfoFailed");
        assert_eq!(error.message(), expected);
    }
}

#[test]
fn msvc_debug_sources_emit_codeview_module_flags() {
    let artifact = unit_run(64);
    let directory = tempfile::tempdir().expect("temp directory");
    let object = directory.path().join("debug.obj");
    let ir_path = directory.path().join("debug-msvc.ll");
    let options = NativeObjectOptions {
        emit_ir: Some(ir_path.clone()),
        debug_sources: vec![DebugSource::new(
            0,
            "src/main.loom",
            "fn main() Unit { Unit }\n",
        )],
        target_triple: Some("x86_64-pc-windows-msvc".to_owned()),
        ..NativeObjectOptions::default()
    };
    emit_lcir_native_object(&artifact, &object, &options).expect("emit MSVC LCIR object");
    assert!(object.is_file());
    let ir = std::fs::read_to_string(ir_path).expect("read MSVC debug IR");
    assert!(ir.contains("CodeView"), "{ir}");
    assert!(!ir.contains("Dwarf Version"), "{ir}");
    assert!(ir.contains("!DICompileUnit"), "{ir}");
    assert!(ir.contains("!DISubprogram"), "{ir}");
    assert!(ir.contains("name: \"main\""), "{ir}");
    assert!(ir.contains("!DISubroutineType"), "{ir}");
    assert!(ir.contains("!DILocation"), "{ir}");
}
