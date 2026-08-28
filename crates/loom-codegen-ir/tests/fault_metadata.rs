use loom_codegen_ir::{
    ARTIFACT_IDENTITY_SCHEMA, ArtifactRootRequest, CONTRACT_FAULT_TEXT_MAX_BYTES,
    ContractFaultBlame, ContractFaultKind, ContractFaultMetadata, Effects, FaultMetadata, Origin,
    Program, ProgramBuilder, Signature, TargetLayout, Terminator, TerminatorKind, ValidationCode,
    artifact_identity, dump_program,
};
use loom_core::{FileId, Span};
use loom_mir::{FunctionId as MirFunctionId, Type};

fn origin(source: u32) -> Origin {
    Origin::synthetic(MirFunctionId(source))
}

fn raw_fault_program(metadata: ContractFaultMetadata) -> Program {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target layout"));
    let unit = builder.type_id(&Type::Unit).expect("Unit type");
    let root = builder
        .declare_function(
            origin(200),
            "fault.metadata",
            Signature::new(Vec::new(), unit),
            Effects::MAY_FAULT,
        )
        .expect("declare root");
    {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry block");
        function.set_entry(entry).expect("set entry");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Fault {
                        metadata: FaultMetadata::contract(metadata),
                    },
                    origin(200),
                ),
            )
            .expect("fault terminator");
    }
    builder.finish()
}

fn validation_errors(metadata: ContractFaultMetadata) -> Vec<(ValidationCode, String, String)> {
    raw_fault_program(metadata)
        .into_checked()
        .expect_err("forged metadata must be rejected")
        .as_slice()
        .iter()
        .map(|error| {
            (
                error.code(),
                error.path().to_owned(),
                error.message().to_owned(),
            )
        })
        .collect()
}

fn checked_fault_artifact(metadata: ContractFaultMetadata) -> loom_codegen_ir::CheckedArtifact {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target layout"));
    let unit = builder.type_id(&Type::Unit).expect("Unit type");
    let root = builder
        .declare_function(
            origin(201),
            "fault.identity",
            Signature::new(Vec::new(), unit),
            Effects::MAY_FAULT,
        )
        .expect("declare root");
    {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry block");
        function.set_entry(entry).expect("set entry");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Fault {
                        metadata: FaultMetadata::contract(metadata),
                    },
                    origin(201),
                ),
            )
            .expect("fault terminator");
    }
    builder
        .finish_checked()
        .expect("canonical metadata")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("closed artifact")
}

#[test]
fn canonical_fault_kinds_have_stable_ordered_dump_vocabulary() {
    let assertion_span = Span::new(FileId(1), 2, 3);
    let contract_span = Span::new(FileId(4), 5, 6);
    let call_span = Span::new(FileId(7), 8, 9);
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target layout"));
    let unit = builder.type_id(&Type::Unit).expect("Unit type");
    let metadata = [
        ContractFaultMetadata::contract(
            ContractFaultKind::Precondition,
            "amount.positive",
            contract_span,
            call_span,
        ),
        ContractFaultMetadata::contract(
            ContractFaultKind::Postcondition,
            "result.positive",
            contract_span,
            contract_span,
        ),
        ContractFaultMetadata::contract(
            ContractFaultKind::Invariant,
            "balance.nonnegative",
            contract_span,
            contract_span,
        ),
        ContractFaultMetadata::assertion(assertion_span),
    ];

    for (index, metadata) in metadata.into_iter().enumerate() {
        let source = u32::try_from(210 + index).expect("source id");
        let function_id = builder
            .declare_function(
                origin(source),
                format!("fault.kind.{index}"),
                Signature::new(Vec::new(), unit),
                Effects::MAY_FAULT,
            )
            .expect("declare function");
        let mut function = builder.function(function_id).expect("function builder");
        let entry = function.create_block().expect("entry block");
        function.set_entry(entry).expect("set entry");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::Fault {
                        metadata: FaultMetadata::contract(metadata),
                    },
                    origin(source),
                ),
            )
            .expect("fault terminator");
    }

    let checked = builder.finish_checked().expect("canonical fault metadata");
    let first = dump_program(&checked);
    assert_eq!(first, dump_program(&checked));
    assert!(first.starts_with("lcir 32\n"), "{first}");
    for expected in [
        "fault contract PreconditionFault category=precondition user_code=\"amount.positive\" message=\"contract `amount.positive` was not satisfied\" contract_span=file4:5..6 blame_span=file7:8..9",
        "fault contract PostconditionFault category=postcondition user_code=\"result.positive\" message=\"contract `result.positive` was not satisfied\" contract_span=file4:5..6 blame_span=file4:5..6",
        "fault contract InvariantFault category=invariant user_code=\"balance.nonnegative\" message=\"contract `balance.nonnegative` was not satisfied\" contract_span=file4:5..6 blame_span=file4:5..6",
        "fault contract AssertionFault category=assertion user_code=none message=\"assertion was not satisfied\" contract_span=file1:2..3 blame_span=file1:2..3",
    ] {
        assert!(
            first.contains(expected),
            "missing `{expected}` in:\n{first}"
        );
    }
    assert!(!first.contains("AssertionFailed"), "{first}");
    assert!(!first.contains("ContractFailed"), "{first}");
}

#[test]
fn independent_validation_rejects_forged_assertion_metadata() {
    let errors = validation_errors(ContractFaultMetadata::new(
        ContractFaultKind::Assertion,
        Some("forged.name".into()),
        "forged message",
        Span::new(FileId(1), 20, 10),
        Span::new(FileId(2), 30, 40),
    ));

    for suffix in ["contract_span", "user_code", "message", "blame_span"] {
        assert!(
            errors.iter().any(|(code, path, _)| {
                *code == ValidationCode::FaultMetadata && path.ends_with(suffix)
            }),
            "missing metadata error for {suffix}: {errors:#?}"
        );
    }
}

#[test]
fn independent_validation_rejects_forged_named_contract_metadata() {
    let contract_span = Span::new(FileId(3), 10, 20);
    let errors = validation_errors(ContractFaultMetadata::new(
        ContractFaultKind::Postcondition,
        Some(String::new()),
        "not derived from the user code",
        contract_span,
        Span::new(FileId(4), 30, 40),
    ));

    for suffix in ["user_code", "message", "blame_span"] {
        assert!(
            errors.iter().any(|(code, path, _)| {
                *code == ValidationCode::FaultMetadata && path.ends_with(suffix)
            }),
            "missing metadata error for {suffix}: {errors:#?}"
        );
    }
}

#[test]
fn precondition_accepts_a_concrete_distinct_call_site_blame_span() {
    raw_fault_program(ContractFaultMetadata::contract(
        ContractFaultKind::Precondition,
        "input.valid",
        Span::new(FileId(5), 10, 20),
        Span::new(FileId(6), 30, 40),
    ))
    .into_checked()
    .expect("precondition blame is its concrete call site");
}

#[test]
fn coroutine_call_site_blame_is_rejected_outside_a_carrying_coroutine() {
    let errors = validation_errors(ContractFaultMetadata::coroutine_precondition(
        "input.valid",
        Span::new(FileId(6), 10, 20),
    ));

    assert!(errors.iter().any(|(code, path, message)| {
        *code == ValidationCode::FaultMetadata
            && path.ends_with("blame_span")
            && message.contains("requires a coroutine plan carrying the caller span")
    }));
}

#[test]
fn coroutine_call_site_blame_is_rejected_for_non_preconditions() {
    let contract_span = Span::new(FileId(7), 10, 20);
    let errors = validation_errors(ContractFaultMetadata::new(
        ContractFaultKind::Postcondition,
        Some("output.valid".into()),
        "contract `output.valid` was not satisfied",
        contract_span,
        ContractFaultBlame::CoroutineCallSite,
    ));

    assert!(errors.iter().any(|(code, path, message)| {
        *code == ValidationCode::FaultMetadata
            && path.ends_with("blame_span")
            && message.contains("only PreconditionFault")
    }));
}

#[test]
fn every_non_precondition_fault_rejects_a_distinct_blame_span() {
    let contract_span = Span::new(FileId(10), 10, 20);
    let blame_span = Span::new(FileId(11), 30, 40);
    let forged = [
        ContractFaultMetadata::new(
            ContractFaultKind::Postcondition,
            Some("output.valid".into()),
            "contract `output.valid` was not satisfied",
            contract_span,
            blame_span,
        ),
        ContractFaultMetadata::new(
            ContractFaultKind::Invariant,
            Some("state.valid".into()),
            "contract `state.valid` was not satisfied",
            contract_span,
            blame_span,
        ),
        ContractFaultMetadata::new(
            ContractFaultKind::Assertion,
            None,
            "assertion was not satisfied",
            contract_span,
            blame_span,
        ),
    ];

    for metadata in forged {
        let errors = validation_errors(metadata);
        assert!(
            errors.iter().any(|(code, path, _)| {
                *code == ValidationCode::FaultMetadata && path.ends_with("blame_span")
            }),
            "distinct blame span was accepted: {errors:#?}"
        );
    }
}

#[test]
fn independent_validation_bounds_hostile_fault_text_before_encoding() {
    let span = Span::new(FileId(12), 10, 20);
    let oversized_code = "é".repeat(CONTRACT_FAULT_TEXT_MAX_BYTES / 2 + 1);
    let code_errors = validation_errors(ContractFaultMetadata::new(
        ContractFaultKind::Precondition,
        Some(oversized_code),
        "already forged",
        span,
        span,
    ));
    assert!(code_errors.iter().any(|(code, path, message)| {
        *code == ValidationCode::FaultMetadata
            && path.ends_with("user_code")
            && message.contains("UTF-8 bytes")
    }));

    let message_errors = validation_errors(ContractFaultMetadata::new(
        ContractFaultKind::Assertion,
        None,
        "x".repeat(CONTRACT_FAULT_TEXT_MAX_BYTES + 1),
        span,
        span,
    ));
    assert!(message_errors.iter().any(|(code, path, message)| {
        *code == ValidationCode::FaultMetadata
            && path.ends_with("message")
            && message.contains("exceeding")
    }));

    let derived_errors = validation_errors(ContractFaultMetadata::contract(
        ContractFaultKind::Invariant,
        "x".repeat(CONTRACT_FAULT_TEXT_MAX_BYTES),
        span,
        span,
    ));
    assert!(derived_errors.iter().any(|(code, path, message)| {
        *code == ValidationCode::FaultMetadata
            && path.ends_with("message")
            && message.contains("exceeding")
    }));
}

#[test]
fn contract_metadata_is_part_of_artifact_identity() {
    assert_eq!(ARTIFACT_IDENTITY_SCHEMA, 33);
    let contract_span = Span::new(FileId(7), 10, 20);
    let first = checked_fault_artifact(ContractFaultMetadata::contract(
        ContractFaultKind::Precondition,
        "input.valid",
        contract_span,
        Span::new(FileId(8), 30, 40),
    ));
    let second = checked_fault_artifact(ContractFaultMetadata::contract(
        ContractFaultKind::Precondition,
        "input.valid",
        contract_span,
        Span::new(FileId(8), 31, 40),
    ));

    assert_ne!(
        dump_program(first.program()),
        dump_program(second.program())
    );
    assert_ne!(artifact_identity(&first), artifact_identity(&second));
}
