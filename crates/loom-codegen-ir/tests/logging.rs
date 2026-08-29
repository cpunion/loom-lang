use loom_codegen_ir::{
    Effects, InstructionKind, ManagedSafepoint, Origin, ProgramBuilder, ResultTarget, Signature,
    TargetLayout, Terminator, TerminatorKind, UnwindTarget, ValidationCode, ValueDefinition,
    dump_program, plan_managed_roots, validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

const LOG_LEVEL_TYPE: TypeId = TypeId(20);
const TEXT_MAP_TYPE: TypeId = TypeId(15);

fn origin(source: u32) -> Origin {
    Origin::synthetic(FunctionId(source))
}

fn register_logging_types(
    builder: &mut ProgramBuilder,
) -> (
    loom_codegen_ir::ValueTypeId,
    loom_codegen_ir::ValueTypeId,
    loom_codegen_ir::ValueTypeId,
) {
    let text = builder.add_managed_text_type().expect("managed Text");
    let fields = builder
        .add_managed_text_map_type(Type::Nominal(TEXT_MAP_TYPE, vec![Type::Text]))
        .expect("canonical TextMap[Text]");
    let level = builder
        .add_sum_type(
            Type::Nominal(LOG_LEVEL_TYPE, Vec::new()),
            &[Box::new([]), Box::new([]), Box::new([]), Box::new([])],
        )
        .expect("canonical LogLevel");
    (level, text, fields)
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one forged IR fixture keeps fault-edge shape, dump vocabulary, and managed-root behavior together"
)]
fn typed_log_write_has_exact_fault_edges_and_keeps_managed_operands_live() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let (level, text, fields) = register_logging_types(&mut builder);
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let root = builder
        .declare_function(
            origin(300),
            "logging.write",
            Signature::new([level, text, fields], unit),
            Effects::MAY_FAULT
                .union(Effects::MAY_COLLECT)
                .with_implications(),
        )
        .expect("function");
    let (message, fields_value, scratch) = {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let fault = function.create_block().expect("fault");
        function.set_entry(entry).expect("set entry");
        let level_value = function
            .append_block_parameter(entry, level)
            .expect("level");
        let message = function
            .append_block_parameter(entry, text)
            .expect("message");
        let fields_value = function
            .append_block_parameter(entry, fields)
            .expect("fields");
        let scratch = function
            .append_instruction(
                entry,
                InstructionKind::TextConcat {
                    left: message,
                    right: message,
                },
                &[text],
                origin(300),
            )
            .expect("collecting operation")[0];
        let result = function
            .append_block_parameter(normal, unit)
            .expect("Unit result");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::LogWrite {
                        level: level_value,
                        message,
                        fields: fields_value,
                        normal: ResultTarget::new(normal, Vec::new()),
                        fault: UnwindTarget::new(fault, Vec::new()),
                    },
                    origin(300),
                ),
            )
            .expect("log write");
        function
            .terminate(
                normal,
                Terminator::new(TerminatorKind::Return(result), origin(300)),
            )
            .expect("return");
        function
            .terminate(
                fault,
                Terminator::new(TerminatorKind::ResumeFault, origin(300)),
            )
            .expect("resume fault");
        (message, fields_value, scratch)
    };

    let checked = builder
        .finish_checked()
        .expect("checked typed logging LCIR");
    let dump = dump_program(&checked);
    assert!(dump.contains("log.write "), "{dump}");
    assert!(dump.contains("fields %"), "{dump}");
    assert!(dump.contains(", normal "), "{dump}");
    assert!(dump.contains(", fault "), "{dump}");

    let ValueDefinition::InstructionResult { instruction, .. } = checked
        .as_program()
        .function(root)
        .and_then(|function| function.value(scratch))
        .expect("scratch Text")
        .definition()
    else {
        panic!("scratch Text must be an instruction result")
    };
    let roots = plan_managed_roots(&checked, root).expect("managed-root plan");
    assert_eq!(roots.slots().len(), 2, "{roots:?}");
    assert!(roots.slots().iter().any(|slot| slot.value() == message));
    assert!(
        roots
            .slots()
            .iter()
            .any(|slot| slot.value() == fields_value)
    );
    assert!(
        roots
            .state(ManagedSafepoint::Instruction(instruction))
            .is_some()
    );
    let log_block = checked
        .as_program()
        .function(root)
        .expect("root function")
        .entry()
        .expect("entry");
    assert_eq!(
        roots.state(ManagedSafepoint::Terminator(log_block)),
        None,
        "log.write is fallible but is not a moving-GC safepoint"
    );
}

#[test]
fn independent_validation_rejects_noncanonical_log_operands() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let (_level, _text, _fields) = register_logging_types(&mut builder);
    let wrong_fields = builder
        .add_managed_text_map_type(Type::Nominal(TEXT_MAP_TYPE, vec![Type::Int]))
        .expect("noncanonical TextMap[Int]");
    let bool_ty = builder.type_id(&Type::Bool).expect("Bool");
    let int_ty = builder.type_id(&Type::Int).expect("Int");
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let root = builder
        .declare_function(
            origin(301),
            "logging.forged",
            Signature::new([bool_ty, int_ty, wrong_fields], unit),
            Effects::MAY_FAULT,
        )
        .expect("function");
    {
        let mut function = builder.function(root).expect("function builder");
        let entry = function.create_block().expect("entry");
        let normal = function.create_block().expect("normal");
        let fault = function.create_block().expect("fault");
        function.set_entry(entry).expect("set entry");
        let wrong_level = function
            .append_block_parameter(entry, bool_ty)
            .expect("wrong level");
        let wrong_message = function
            .append_block_parameter(entry, int_ty)
            .expect("wrong message");
        let wrong_fields = function
            .append_block_parameter(entry, wrong_fields)
            .expect("wrong fields");
        let result = function
            .append_block_parameter(normal, unit)
            .expect("Unit result");
        function
            .terminate(
                entry,
                Terminator::new(
                    TerminatorKind::LogWrite {
                        level: wrong_level,
                        message: wrong_message,
                        fields: wrong_fields,
                        normal: ResultTarget::new(normal, Vec::new()),
                        fault: UnwindTarget::new(fault, Vec::new()),
                    },
                    origin(301),
                ),
            )
            .expect("forged log write");
        function
            .terminate(
                normal,
                Terminator::new(TerminatorKind::Return(result), origin(301)),
            )
            .expect("return");
        function
            .terminate(
                fault,
                Terminator::new(TerminatorKind::ResumeFault, origin(301)),
            )
            .expect("resume fault");
    }

    let errors = validate_program(&builder.finish()).expect_err("forged logging must fail");
    for operand in [".level", ".message", ".fields"] {
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code() == ValidationCode::TypeMismatch && error.path().ends_with(operand)
            }),
            "missing {operand} type error: {:#?}",
            errors.as_slice()
        );
    }
}
