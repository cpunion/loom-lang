use loom_codegen_ir::{
    CanonicalTypeCatalog, Constant, Effects, InstructionKind, Origin, Program, ProgramBuilder,
    Signature, TargetLayout, Terminator, TerminatorKind, ValidationCode, ValueTypeId, dump_program,
    validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

fn origin(source: u32) -> Origin {
    Origin::synthetic(FunctionId(source))
}

fn task_tuple_program(reuse_aggregate: bool) -> Program {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let integer = builder.type_id(&Type::Int).expect("Int");
    let task_semantic = Type::Task(Box::new(Type::Int));
    let task = builder
        .add_task_handle_type(task_semantic.clone())
        .expect("Task[Int]");
    let tuple = builder
        .add_tuple_type(&[task_semantic, Type::Int])
        .expect("(Task[Int], Int)");
    let function_id = builder
        .declare_function(
            origin(1),
            "product_split.task_tuple",
            Signature::new([tuple], task),
            Effects::NONE,
        )
        .expect("function");
    {
        let mut function = builder.function(function_id).expect("function builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let aggregate = function
            .append_block_parameter(entry, tuple)
            .expect("tuple parameter");
        let fields = function
            .append_instruction(
                entry,
                InstructionKind::ProductSplit { aggregate },
                &[task, integer],
                origin(1),
            )
            .expect("split");
        if reuse_aggregate {
            function
                .append_instruction(
                    entry,
                    InstructionKind::ProductExtract {
                        aggregate,
                        field: 1,
                    },
                    &[integer],
                    origin(1),
                )
                .expect("reuse consumed aggregate");
        }
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(fields[0]), origin(1)),
            )
            .expect("return Task");
    }
    builder.finish()
}

#[test]
fn product_split_atomically_moves_a_task_tuple_and_publishes_each_field() {
    let checked = task_tuple_program(false)
        .into_checked()
        .unwrap_or_else(|errors| panic!("valid Task tuple split failed: {errors:#?}"));
    let dump = dump_program(&checked);
    assert!(dump.contains("product.split"), "{dump}");

    let errors = validate_program(&task_tuple_program(true))
        .expect_err("a consumed affine aggregate was reused");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidTaskOwnership
            && error.message().contains("borrowed after it was consumed")
    }));
}

fn add_unit_split_function(
    builder: &mut ProgramBuilder,
    source: u32,
    name: &str,
    aggregate_type: ValueTypeId,
    result_types: &[ValueTypeId],
) {
    let unit = builder.type_id(&Type::Unit).expect("Unit");
    let function_id = builder
        .declare_function(
            origin(source),
            name,
            Signature::new([aggregate_type], unit),
            Effects::NONE,
        )
        .expect("function");
    let mut function = builder.function(function_id).expect("function builder");
    let entry = function.create_block().expect("entry");
    function.set_entry(entry).expect("set entry");
    let aggregate = function
        .append_block_parameter(entry, aggregate_type)
        .expect("aggregate parameter");
    function
        .append_instruction(
            entry,
            InstructionKind::ProductSplit { aggregate },
            result_types,
            origin(source),
        )
        .expect("unchecked split");
    let unit_value = function
        .append_instruction(
            entry,
            InstructionKind::Constant(Constant::Unit),
            &[unit],
            origin(source),
        )
        .expect("Unit")[0];
    function
        .terminate(
            entry,
            Terminator::new(TerminatorKind::Return(unit_value), origin(source)),
        )
        .expect("return");
}

#[test]
fn product_split_requires_an_exact_ordinary_structural_tuple() {
    let file_identity = TypeId(204);
    let mut builder = ProgramBuilder::with_canonical_types(
        TargetLayout::new(64).expect("target"),
        CanonicalTypeCatalog {
            file: Some(file_identity),
            ..CanonicalTypeCatalog::default()
        },
    );
    let integer = builder.type_id(&Type::Int).expect("Int");
    let boolean = builder.type_id(&Type::Bool).expect("Bool");
    let tuple_semantic = Type::Tuple(vec![Type::Int, Type::Bool]);
    let tuple = builder
        .add_tuple_type(&[Type::Int, Type::Bool])
        .expect("tuple");
    let nominal = builder
        .add_pod_record_type(
            Type::Nominal(TypeId(201), Vec::new()),
            &[Type::Int, Type::Bool],
        )
        .expect("nominal product");
    let invariant = builder
        .add_invariant_record_type(
            Type::Nominal(TypeId(202), Vec::new()),
            &[Type::Int, Type::Bool],
        )
        .expect("invariant product");
    let transparent = builder
        .add_transparent_type(Type::Nominal(TypeId(203), Vec::new()), &tuple_semantic)
        .expect("transparent tuple");
    let file = builder
        .add_pod_record_type(Type::Nominal(file_identity, Vec::new()), &[Type::Int])
        .expect("File capability");

    add_unit_split_function(&mut builder, 10, "split.bad_shape", tuple, &[boolean]);
    for (source, name, ty, fields) in [
        (11, "split.nominal", nominal, vec![integer, boolean]),
        (12, "split.invariant", invariant, vec![integer, boolean]),
        (13, "split.transparent", transparent, vec![integer, boolean]),
        (14, "split.resource", file, vec![integer]),
    ] {
        add_unit_split_function(&mut builder, source, name, ty, &fields);
    }

    let errors = validate_program(&builder.finish()).expect_err("invalid splits were accepted");
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InstructionShape
            && error.message().contains("operation requires 2")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error.message().contains("ordinary direct structural tuple")
    }));
    assert!(errors.as_slice().iter().any(|error| {
        error.code() == ValidationCode::TypeMismatch
            && error
                .message()
                .contains("resource capabilities cannot be split")
    }));
}
