use loom_codegen_ir::{
    AwaitMode, BlockTarget, CanonicalTypeCatalog, Constant, CoroutinePlan, CoroutineSuspension,
    Effects, InstructionKind, IoTaskOperation, Origin, Program, ProgramBuilder, ResultTarget,
    Signature, TargetLayout, Terminator, TerminatorKind, UnwindTarget, ValidationCode,
    validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

const RESULT_TYPE_ID: TypeId = TypeId(101);
const FILE_TYPE_ID: TypeId = TypeId(107);
const SOCKET_TYPE_ID: TypeId = TypeId(108);
const IO_ERROR_TYPE_ID: TypeId = TypeId(116);
const IO_ERROR_KIND_TYPE_ID: TypeId = TypeId(117);

fn io_catalog() -> CanonicalTypeCatalog {
    CanonicalTypeCatalog {
        result: Some(RESULT_TYPE_ID),
        file: Some(FILE_TYPE_ID),
        socket: Some(SOCKET_TYPE_ID),
        io_error: Some(IO_ERROR_TYPE_ID),
        io_error_kind: Some(IO_ERROR_KIND_TYPE_ID),
        ..CanonicalTypeCatalog::default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Defect {
    None,
    MissingArgument,
    WrongArgumentType,
    FileShape,
    SocketShape,
    IoErrorShape,
    IoErrorInvariantProduct,
    IoErrorKindTag,
    MissingResultRegistration,
    MissingTaskRegistration,
    ResultShape,
    ResultTag,
    WrongTaskType,
    MissingExecutorEffect,
    CancellationCleanup,
}

#[derive(Clone, Copy, Debug)]
enum ResourceProductOperation {
    Construct,
    Insert,
    Extract,
}

fn nominal(id: TypeId) -> Type {
    Type::Nominal(id, Vec::new())
}

fn empty_variants(count: usize) -> Vec<Box<[Type]>> {
    vec![Box::new([]); count]
}

#[expect(
    clippy::too_many_lines,
    reason = "one hostile raw-LCIR fixture keeps every nominal and physical typed-I/O boundary independently forgeable"
)]
fn io_task_program(operation: IoTaskOperation, defect: Defect) -> Program {
    let origin = Origin::synthetic(FunctionId(500));
    let mut builder =
        ProgramBuilder::with_canonical_types(TargetLayout::new(64).expect("target"), io_catalog());
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let text = builder
        .add_managed_text_type()
        .expect("canonical managed Text");

    let file_semantic = nominal(FILE_TYPE_ID);
    let file = builder
        .add_pod_record_type(
            file_semantic.clone(),
            &[if defect == Defect::FileShape {
                Type::Bool
            } else {
                Type::Int
            }],
        )
        .expect("File-shaped record");
    let socket_semantic = nominal(SOCKET_TYPE_ID);
    let socket = builder
        .add_pod_record_type(
            socket_semantic.clone(),
            &[if defect == Defect::SocketShape {
                Type::Bool
            } else {
                Type::Int
            }],
        )
        .expect("Socket-shaped record");

    let io_error_kind_semantic = nominal(IO_ERROR_KIND_TYPE_ID);
    let kind_variants = empty_variants(if defect == Defect::IoErrorKindTag {
        1
    } else {
        10
    });
    builder
        .add_sum_type(io_error_kind_semantic.clone(), &kind_variants)
        .expect("IoErrorKind-shaped sum");
    let io_error_semantic = nominal(IO_ERROR_TYPE_ID);
    let io_error_fields = if defect == Defect::IoErrorShape {
        vec![Type::Text, io_error_kind_semantic]
    } else {
        vec![io_error_kind_semantic, Type::Text]
    };
    if defect == Defect::IoErrorInvariantProduct {
        builder
            .add_invariant_record_type(io_error_semantic.clone(), &io_error_fields)
            .expect("invariant IoError-shaped record");
    } else {
        builder
            .add_pod_record_type(io_error_semantic.clone(), &io_error_fields)
            .expect("IoError-shaped record");
    }

    let success_semantic = match operation {
        IoTaskOperation::FileOpenRead | IoTaskOperation::FileCreate => file_semantic,
        IoTaskOperation::FileReadText | IoTaskOperation::SocketReadText => Type::Text,
        IoTaskOperation::FileWriteText | IoTaskOperation::SocketWriteText => Type::Unit,
        IoTaskOperation::SocketConnect => socket_semantic,
    };
    let result_semantic = Type::Nominal(
        RESULT_TYPE_ID,
        vec![success_semantic.clone(), io_error_semantic.clone()],
    );
    let result_variants = match defect {
        Defect::ResultShape => vec![
            Box::from([io_error_semantic.clone()]),
            Box::from([success_semantic.clone()]),
        ],
        Defect::ResultTag => {
            vec![Box::from([success_semantic.clone()])]
        }
        _ => vec![
            Box::from([success_semantic.clone()]),
            Box::from([io_error_semantic.clone()]),
        ],
    };
    let result = (defect != Defect::MissingResultRegistration).then(|| {
        builder
            .add_sum_type(result_semantic.clone(), &result_variants)
            .expect("Result-shaped sum")
    });
    let task_result = (!matches!(
        defect,
        Defect::MissingResultRegistration | Defect::MissingTaskRegistration
    ))
    .then(|| {
        builder
            .add_task_handle_type(Type::Task(Box::new(result_semantic)))
            .expect("Task[Result[T, IoError]]")
    });
    let wrong_task = if matches!(
        defect,
        Defect::WrongTaskType | Defect::MissingResultRegistration | Defect::MissingTaskRegistration
    ) {
        Some(
            builder
                .add_task_handle_type(Type::Task(Box::new(Type::Bool)))
                .expect("wrong Task[Bool]"),
        )
    } else {
        None
    };

    let effects = match defect {
        Defect::MissingExecutorEffect => Effects::NONE,
        Defect::CancellationCleanup => Effects::MAY_FAULT
            .union(Effects::MAY_SUSPEND)
            .with_implications(),
        _ => Effects::NEEDS_EXECUTOR.with_implications(),
    };
    let root = builder
        .declare_function(
            origin,
            "typed_io.raw",
            Signature::new([text, file, socket, integer], unit),
            effects,
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        if defect == Defect::CancellationCleanup {
            function
                .set_coroutine_plan(CoroutinePlan::new(
                    unit,
                    [CoroutineSuspension::new(
                        1,
                        AwaitMode::All,
                        [result.expect("cancellation test has Result")],
                        [],
                    )],
                ))
                .expect("typed I/O cancellation coroutine plan");
        }
        let entry = function.create_block().expect("entry");
        let normal = (defect == Defect::CancellationCleanup)
            .then(|| function.create_block().expect("normal"));
        let fault = (defect == Defect::CancellationCleanup)
            .then(|| function.create_block().expect("fault"));
        let cancel = (defect == Defect::CancellationCleanup)
            .then(|| function.create_block().expect("cancel"));
        function.set_entry(entry).expect("set entry");
        let text = function
            .append_block_parameter(entry, text)
            .expect("Text parameter");
        let file = function
            .append_block_parameter(entry, file)
            .expect("File parameter");
        let socket = function
            .append_block_parameter(entry, socket)
            .expect("Socket parameter");
        let integer = function
            .append_block_parameter(entry, integer)
            .expect("Int parameter");
        let arguments = match operation {
            IoTaskOperation::FileOpenRead | IoTaskOperation::FileCreate => vec![text],
            IoTaskOperation::FileReadText => vec![file],
            IoTaskOperation::FileWriteText if defect == Defect::MissingArgument => vec![file],
            IoTaskOperation::FileWriteText => vec![file, text],
            IoTaskOperation::SocketConnect if defect == Defect::WrongArgumentType => {
                vec![text, text]
            }
            IoTaskOperation::SocketConnect => vec![text, integer],
            IoTaskOperation::SocketReadText => vec![socket],
            IoTaskOperation::SocketWriteText => vec![socket, text],
        };
        let task = function
            .append_instruction(
                entry,
                InstructionKind::IoTaskCreate {
                    operation,
                    arguments: arguments.clone().into_boxed_slice(),
                },
                &[wrong_task
                    .or(task_result)
                    .expect("test has an output type for the forged instruction")],
                origin,
            )
            .expect("unchecked typed I/O Task")[0];
        if let (Some(normal), Some(fault), Some(cancel)) = (normal, fault, cancel) {
            function
                .append_block_parameter(normal, result.expect("cancellation test has Result"))
                .expect("typed I/O Result");
            function
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::AwaitTasks {
                            state: 1,
                            mode: AwaitMode::All,
                            tasks: Box::from([task]),
                            normal: ResultTarget::new(normal, []),
                            fault: UnwindTarget::new(fault, []),
                            cancel: BlockTarget::new(cancel, []),
                        },
                        origin,
                    ),
                )
                .expect("await typed I/O Task");
            function
                .append_instruction(
                    cancel,
                    InstructionKind::IoTaskCreate {
                        operation,
                        arguments: arguments.into_boxed_slice(),
                    },
                    &[task_result.expect("cancellation test has canonical Task")],
                    origin,
                )
                .expect("forged cancellation I/O Task");
            function
                .terminate(
                    cancel,
                    Terminator::new(TerminatorKind::TaskCancelled, origin),
                )
                .expect("propagate cancellation");
            function
                .terminate(fault, Terminator::new(TerminatorKind::ResumeFault, origin))
                .expect("propagate fault");
            let returned = function
                .append_instruction(
                    normal,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit],
                    origin,
                )
                .expect("Unit")[0];
            function
                .terminate(
                    normal,
                    Terminator::new(TerminatorKind::Return(returned), origin),
                )
                .expect("return");
            return builder.finish();
        }
        let returned = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(returned), origin),
            )
            .expect("return");
    }
    builder.finish()
}

fn rejected(operation: IoTaskOperation, defect: Defect) -> loom_codegen_ir::ValidationErrors {
    validate_program(&io_task_program(operation, defect))
        .expect_err("malformed typed I/O LCIR must fail closed")
}

fn resource_product_program(
    resource_type_id: TypeId,
    operation: ResourceProductOperation,
) -> Program {
    let origin = Origin::synthetic(FunctionId(501));
    let mut builder =
        ProgramBuilder::with_canonical_types(TargetLayout::new(64).expect("target"), io_catalog());
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let integer = builder.type_id(&Type::Int).expect("Int");
    let resource = builder
        .add_pod_record_type(nominal(resource_type_id), &[Type::Int])
        .expect("canonical resource capability");
    let root = builder
        .declare_function(
            origin,
            "typed_io.forge_resource",
            Signature::new([resource, integer], unit),
            Effects::NONE,
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let resource_value = function
            .append_block_parameter(entry, resource)
            .expect("resource parameter");
        let integer_value = function
            .append_block_parameter(entry, integer)
            .expect("integer parameter");
        match operation {
            ResourceProductOperation::Construct => {
                function
                    .append_instruction(
                        entry,
                        InstructionKind::ProductConstruct {
                            fields: Box::from([integer_value]),
                        },
                        &[resource],
                        origin,
                    )
                    .expect("unchecked resource construction");
            }
            ResourceProductOperation::Insert => {
                function
                    .append_instruction(
                        entry,
                        InstructionKind::ProductInsert {
                            aggregate: resource_value,
                            field: 0,
                            value: integer_value,
                        },
                        &[resource],
                        origin,
                    )
                    .expect("unchecked resource insertion");
            }
            ResourceProductOperation::Extract => {
                function
                    .append_instruction(
                        entry,
                        InstructionKind::ProductExtract {
                            aggregate: resource_value,
                            field: 0,
                        },
                        &[integer],
                        origin,
                    )
                    .expect("unchecked resource extraction");
            }
        }
        let returned = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit],
                origin,
            )
            .expect("Unit")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(returned), origin),
            )
            .expect("return");
    }
    builder.finish()
}

#[test]
fn canonical_typed_io_task_shapes_validate_for_every_closed_operation() {
    for operation in [
        IoTaskOperation::FileOpenRead,
        IoTaskOperation::FileCreate,
        IoTaskOperation::FileReadText,
        IoTaskOperation::FileWriteText,
        IoTaskOperation::SocketConnect,
        IoTaskOperation::SocketReadText,
        IoTaskOperation::SocketWriteText,
    ] {
        validate_program(&io_task_program(operation, Defect::None))
            .unwrap_or_else(|errors| panic!("canonical {operation:?} failed: {errors:#?}"));
    }
}

#[test]
fn typed_io_rejects_wrong_argument_count_and_type() {
    let missing = rejected(IoTaskOperation::FileWriteText, Defect::MissingArgument);
    assert!(missing.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape
            && error.path().ends_with("instruction[0].arguments")
    }));

    let wrong_type = rejected(IoTaskOperation::SocketConnect, Defect::WrongArgumentType);
    assert!(wrong_type.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error.path().ends_with("instruction[0].argument[1]")
    }));
}

#[test]
fn typed_io_rejects_forged_resource_and_io_error_representations() {
    let mut admitted = Vec::new();
    for (operation, defect) in [
        (IoTaskOperation::FileReadText, Defect::FileShape),
        (IoTaskOperation::SocketReadText, Defect::SocketShape),
        (IoTaskOperation::FileCreate, Defect::IoErrorShape),
        (IoTaskOperation::FileCreate, Defect::IoErrorInvariantProduct),
        (IoTaskOperation::FileCreate, Defect::IoErrorKindTag),
    ] {
        match validate_program(&io_task_program(operation, defect)) {
            Ok(()) => admitted.push(defect),
            Err(errors) => assert!(
                errors.as_slice().iter().any(|error| matches!(
                    error.code(),
                    ValidationCode::InstructionShape | ValidationCode::TypeMismatch
                )),
                "typed I/O rejected forged {defect:?} for an unrelated reason: {errors:#?}"
            ),
        }
    }
    assert!(
        admitted.is_empty(),
        "typed I/O admitted forged representations: {admitted:?}"
    );
}

#[test]
fn typed_io_rejects_missing_result_and_task_registrations_explicitly() {
    let missing_result = rejected(
        IoTaskOperation::FileCreate,
        Defect::MissingResultRegistration,
    );
    assert!(missing_result.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape
            && error.path().ends_with("instruction[0].result[0]")
            && error.message().contains("Result[success, IoError]")
            && error.message().contains("registration")
    }));

    let missing_task = rejected(IoTaskOperation::FileCreate, Defect::MissingTaskRegistration);
    assert!(missing_task.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape
            && error.path().ends_with("instruction[0].result[0]")
            && error.message().contains("Task[Result[success, IoError]]")
            && error.message().contains("registration")
    }));
}

#[test]
fn typed_io_rejects_a_layout_compatible_task_with_the_wrong_output() {
    let errors = rejected(IoTaskOperation::FileCreate, Defect::WrongTaskType);
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error.path().ends_with("instruction[0].result[0]")
    }));
}

#[test]
fn general_product_instructions_cannot_forge_mutate_or_expose_resource_tokens() {
    for resource in [FILE_TYPE_ID, SOCKET_TYPE_ID] {
        for operation in [
            ResourceProductOperation::Construct,
            ResourceProductOperation::Insert,
            ResourceProductOperation::Extract,
        ] {
            let errors = validate_program(&resource_product_program(resource, operation))
                .expect_err("general product instruction must not admit a resource token");
            assert!(
                errors.as_slice().iter().any(|error| {
                    error.code() == ValidationCode::TypeMismatch
                        && error.message().contains("opaque tokens")
                }),
                "general {operation:?} rejected resource {resource:?} for an unrelated reason: {errors:#?}"
            );
        }
    }
}

#[test]
fn typed_io_rejects_forged_result_shape_tag_and_task_type() {
    for defect in [
        Defect::ResultShape,
        Defect::ResultTag,
        Defect::WrongTaskType,
    ] {
        let errors = rejected(IoTaskOperation::FileCreate, defect);
        assert!(
            errors.as_slice().iter().any(|error| matches!(
                error.code(),
                ValidationCode::InstructionShape | ValidationCode::TypeMismatch
            )),
            "typed I/O admitted forged {defect:?}: {errors:#?}"
        );
    }
}

#[test]
fn typed_io_requires_the_executor_effect() {
    let errors = rejected(IoTaskOperation::FileCreate, Defect::MissingExecutorEffect);
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::EffectMismatch && error.message().contains("NEEDS_EXECUTOR")
    }));
}

#[test]
fn cancellation_cleanup_cannot_create_a_typed_io_task() {
    let errors = rejected(IoTaskOperation::FileCreate, Defect::CancellationCleanup);
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidCoroutinePlan
            && error
                .message()
                .contains("cancellation cleanup cannot create a typed I/O Task")
    }));
}
