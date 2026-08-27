use loom_codegen_ir::{
    ArtifactRootRequest, BuildErrorCode, Constant, Effects, INSTANCE_KEY_STRUCTURE_BUDGET,
    InstanceKey, InstanceWitnessArgument, InstructionKind, Origin, ProgramBuilder, Signature,
    TargetLayout, Terminator, TerminatorKind, artifact_identity, dump_program,
};
use loom_mir::{FunctionId, Type, WitnessId};

fn declare_unit_instance(
    builder: &mut ProgramBuilder,
    key: InstanceKey,
    name: &str,
) -> loom_codegen_ir::InstanceId {
    let unit = builder.type_id(&Type::Unit).expect("Unit type");
    let origin = Origin::synthetic(key.source());
    let instance = builder
        .declare_instance(
            key,
            origin,
            name,
            Signature::new(Vec::new(), unit),
            Effects::NONE,
        )
        .expect("declare instance");
    let mut function = builder.function(instance).expect("function builder");
    let entry = function.create_block().expect("entry block");
    function.set_entry(entry).expect("set entry");
    let value = function
        .append_instruction(
            entry,
            InstructionKind::Constant(Constant::Unit),
            &[unit],
            origin,
        )
        .expect("Unit constant")[0];
    function
        .terminate(
            entry,
            Terminator::new(TerminatorKind::Return(value), origin),
        )
        .expect("return");
    instance
}

#[test]
fn type_and_witness_arguments_are_distinct_instance_identities() {
    let source = FunctionId(7);
    let int_key = InstanceKey::new(
        source,
        vec![Type::Int],
        vec![InstanceWitnessArgument::Concrete(WitnessId(2))],
    );
    let float_key = InstanceKey::new(
        source,
        vec![Type::Float],
        vec![InstanceWitnessArgument::Concrete(WitnessId(2))],
    );
    let applied_key = InstanceKey::new(
        source,
        vec![Type::Int],
        vec![InstanceWitnessArgument::apply(
            WitnessId(2),
            vec![InstanceWitnessArgument::Concrete(WitnessId(3))],
        )],
    );
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let int_instance = declare_unit_instance(&mut builder, int_key.clone(), "same.int");
    let float_instance = declare_unit_instance(&mut builder, float_key.clone(), "same.float");
    let applied_instance = declare_unit_instance(&mut builder, applied_key.clone(), "same.applied");

    assert_ne!(int_instance, float_instance);
    assert_ne!(int_instance, applied_instance);
    assert_eq!(builder.instances().find(&int_key), Some(int_instance));
    assert_eq!(builder.instances().find(&float_key), Some(float_instance));
    assert_eq!(
        builder.instances().find(&applied_key),
        Some(applied_instance)
    );

    let checked = builder.finish_checked().expect("valid keyed instances");
    assert_eq!(
        checked.as_program().instance_key(int_instance),
        Some(&int_key)
    );
    assert_eq!(
        checked.as_program().instance_key(float_instance),
        Some(&float_key)
    );
    assert_eq!(
        checked.as_program().instance_key(applied_instance),
        Some(&applied_key)
    );
    let dump = dump_program(&checked);
    assert!(
        dump.contains("instance i0 = source=f7 types=[Int] witnesses=[Concrete#2]"),
        "{dump}"
    );
    assert!(
        dump.contains("instance i1 = source=f7 types=[Float] witnesses=[Concrete#2]"),
        "{dump}"
    );
    assert!(
        dump.contains("instance i2 = source=f7 types=[Int] witnesses=[Apply#2[Concrete#3]]"),
        "{dump}"
    );
}

#[test]
fn builder_rejects_duplicate_mismatched_and_oversized_instance_keys() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let key = InstanceKey::new(FunctionId(3), vec![Type::Int], Vec::new());
    declare_unit_instance(&mut builder, key.clone(), "first");
    let unit = builder.type_id(&Type::Unit).expect("Unit type");
    let duplicate = builder
        .declare_instance(
            key,
            Origin::synthetic(FunctionId(3)),
            "duplicate",
            Signature::new(Vec::new(), unit),
            Effects::NONE,
        )
        .expect_err("duplicate key must fail");
    assert_eq!(duplicate.code(), BuildErrorCode::DuplicateInstance);

    let mismatched = builder
        .declare_instance(
            InstanceKey::monomorphic(FunctionId(4)),
            Origin::synthetic(FunctionId(5)),
            "mismatched",
            Signature::new(Vec::new(), unit),
            Effects::NONE,
        )
        .expect_err("source mismatch must fail");
    assert_eq!(mismatched.code(), BuildErrorCode::InstanceSourceMismatch);

    let mut witness = InstanceWitnessArgument::Concrete(WitnessId(0));
    for _ in 0..INSTANCE_KEY_STRUCTURE_BUDGET {
        witness = InstanceWitnessArgument::apply(WitnessId(0), vec![witness]);
    }
    let oversized = builder
        .declare_instance(
            InstanceKey::new(FunctionId(6), Vec::new(), vec![witness]),
            Origin::synthetic(FunctionId(6)),
            "oversized",
            Signature::new(Vec::new(), unit),
            Effects::NONE,
        )
        .expect_err("oversized key must fail");
    assert_eq!(oversized.code(), BuildErrorCode::InstanceKeyStructureBudget);

    let open = builder
        .declare_instance(
            InstanceKey::new(
                FunctionId(6),
                vec![Type::Parameter(0)],
                vec![InstanceWitnessArgument::Parameter(0)],
            ),
            Origin::synthetic(FunctionId(6)),
            "open",
            Signature::new(Vec::new(), unit),
            Effects::NONE,
        )
        .expect_err("open instance keys must fail before entering LCIR");
    assert_eq!(open.code(), BuildErrorCode::OpenInstanceKey);
}

#[test]
fn artifact_identity_includes_the_complete_instance_key() {
    fn artifact(key: InstanceKey) -> loom_codegen_ir::CheckedArtifact {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let root = declare_unit_instance(&mut builder, key, "identity.root");
        builder
            .finish_checked()
            .expect("checked program")
            .into_artifact(ArtifactRootRequest::Run(root))
            .expect("checked artifact")
    }

    let int = artifact(InstanceKey::new(
        FunctionId(11),
        vec![Type::Int],
        vec![InstanceWitnessArgument::Concrete(WitnessId(8))],
    ));
    let float = artifact(InstanceKey::new(
        FunctionId(11),
        vec![Type::Float],
        vec![InstanceWitnessArgument::Concrete(WitnessId(8))],
    ));

    assert_ne!(artifact_identity(&int), artifact_identity(&float));
}
