use loom_codegen_ir::{
    AwaitMode, BlockTarget, Constant, Effects, InstructionKind, Origin, Program, ProgramBuilder,
    Signature, TargetLayout, Terminator, TerminatorKind, ValidationCode, validate_program,
};
use loom_mir::{FunctionId, Type, TypeId};

#[derive(Clone, Copy, Debug)]
enum CarrierCase {
    Append,
    DuplicateChild,
    ReuseAfterJoin,
    CfgAlias,
    ListGet,
    WrongResult,
}

#[expect(
    clippy::too_many_lines,
    reason = "one compact raw fixture keeps valid carrier transfers and hostile ownership variants structurally comparable"
)]
fn carrier_program(case: CarrierCase) -> Program {
    let origin = Origin::synthetic(FunctionId(0));
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let integer = builder.type_id(&Type::Int).expect("Int");
    let task_int_semantic = Type::Task(Box::new(Type::Int));
    let task_int = builder
        .add_task_handle_type(task_int_semantic.clone())
        .expect("Task[Int]");
    let task_list_semantic = Type::List(Box::new(task_int_semantic));
    let task_list = builder
        .add_managed_list_type(task_list_semantic)
        .expect("List[Task[Int]]");
    let int_list_semantic = Type::List(Box::new(Type::Int));
    builder
        .add_managed_list_type(int_list_semantic.clone())
        .expect("List[Int]");
    let joined_task = builder
        .add_task_handle_type(Type::Task(Box::new(int_list_semantic)))
        .expect("Task[List[Int]]");

    let allocates = matches!(
        case,
        CarrierCase::Append | CarrierCase::DuplicateChild | CarrierCase::ReuseAfterJoin
    );
    let effects = if allocates {
        Effects::MAY_COLLECT
            .union(Effects::NEEDS_EXECUTOR)
            .with_implications()
    } else {
        Effects::NEEDS_EXECUTOR.with_implications()
    };
    let params = if matches!(case, CarrierCase::CfgAlias | CarrierCase::ListGet) {
        vec![task_list]
    } else if matches!(case, CarrierCase::WrongResult) {
        Vec::new()
    } else {
        vec![task_int, task_int]
    };
    let parameter_types = params.clone();
    let root = builder
        .declare_function(
            origin,
            format!("task_join_list.{case:?}"),
            Signature::new(params, joined_task),
            effects,
        )
        .expect("root");
    {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("entry");
        let params = parameter_types
            .into_iter()
            .map(|ty| {
                function
                    .append_block_parameter(entry, ty)
                    .expect("parameter")
            })
            .collect::<Vec<_>>();

        let mut return_block = entry;
        let result = match case {
            CarrierCase::Append => {
                let list = function
                    .append_instruction(
                        entry,
                        InstructionKind::ListConstruct {
                            elements: Box::from([params[0]]),
                        },
                        &[task_list],
                        origin,
                    )
                    .expect("task list")[0];
                let length = function
                    .append_instruction(
                        entry,
                        InstructionKind::ListLength { list },
                        &[integer],
                        origin,
                    )
                    .expect("borrowed length")[0];
                let _ = length;
                let appended = function
                    .append_instruction(
                        entry,
                        InstructionKind::ListAppend {
                            list,
                            value: params[1],
                        },
                        &[task_list],
                        origin,
                    )
                    .expect("append")[0];
                function
                    .append_instruction(
                        entry,
                        InstructionKind::TaskJoinList {
                            mode: AwaitMode::All,
                            tasks: appended,
                        },
                        &[joined_task],
                        origin,
                    )
                    .expect("join")[0]
            }
            CarrierCase::DuplicateChild => {
                let list = function
                    .append_instruction(
                        entry,
                        InstructionKind::ListConstruct {
                            elements: Box::from([params[0], params[0]]),
                        },
                        &[task_list],
                        origin,
                    )
                    .expect("aliased task list")[0];
                function
                    .append_instruction(
                        entry,
                        InstructionKind::TaskJoinList {
                            mode: AwaitMode::All,
                            tasks: list,
                        },
                        &[joined_task],
                        origin,
                    )
                    .expect("join")[0]
            }
            CarrierCase::ReuseAfterJoin => {
                let list = function
                    .append_instruction(
                        entry,
                        InstructionKind::ListConstruct {
                            elements: Box::from([params[0]]),
                        },
                        &[task_list],
                        origin,
                    )
                    .expect("task list")[0];
                let joined = function
                    .append_instruction(
                        entry,
                        InstructionKind::TaskJoinList {
                            mode: AwaitMode::All,
                            tasks: list,
                        },
                        &[joined_task],
                        origin,
                    )
                    .expect("join")[0];
                function
                    .append_instruction(
                        entry,
                        InstructionKind::ListLength { list },
                        &[integer],
                        origin,
                    )
                    .expect("use after move");
                joined
            }
            CarrierCase::CfgAlias => {
                let forwarded = function.create_block().expect("forwarded");
                let first = function
                    .append_block_parameter(forwarded, task_list)
                    .expect("first carrier");
                let second = function
                    .append_block_parameter(forwarded, task_list)
                    .expect("second carrier");
                function
                    .terminate(
                        entry,
                        Terminator::new(
                            TerminatorKind::Jump(BlockTarget::new(
                                forwarded,
                                [params[0], params[0]],
                            )),
                            origin,
                        ),
                    )
                    .expect("aliased edge");
                function
                    .append_instruction(
                        forwarded,
                        InstructionKind::TaskJoinList {
                            mode: AwaitMode::All,
                            tasks: second,
                        },
                        &[joined_task],
                        origin,
                    )
                    .expect("consume second carrier");
                let joined = function
                    .append_instruction(
                        forwarded,
                        InstructionKind::TaskJoinList {
                            mode: AwaitMode::All,
                            tasks: first,
                        },
                        &[joined_task],
                        origin,
                    )
                    .expect("consume first carrier")[0];
                return_block = forwarded;
                joined
            }
            CarrierCase::ListGet => {
                let index = function
                    .append_instruction(
                        entry,
                        InstructionKind::Constant(Constant::Int(0)),
                        &[integer],
                        origin,
                    )
                    .expect("index")[0];
                function
                    .append_instruction(
                        entry,
                        InstructionKind::ListGet {
                            list: params[0],
                            index,
                        },
                        &[integer],
                        origin,
                    )
                    .expect("forged task get");
                function
                    .append_instruction(
                        entry,
                        InstructionKind::TaskJoinList {
                            mode: AwaitMode::All,
                            tasks: params[0],
                        },
                        &[joined_task],
                        origin,
                    )
                    .expect("join")[0]
            }
            CarrierCase::WrongResult => {
                let empty = function
                    .append_instruction(
                        entry,
                        InstructionKind::ListConstruct {
                            elements: Box::new([]),
                        },
                        &[task_list],
                        origin,
                    )
                    .expect("empty task list")[0];
                function
                    .append_instruction(
                        entry,
                        InstructionKind::TaskJoinList {
                            mode: AwaitMode::All,
                            tasks: empty,
                        },
                        &[task_int],
                        origin,
                    )
                    .expect("wrong join result")[0]
            }
        };
        function
            .terminate(
                return_block,
                Terminator::new(TerminatorKind::Return(result), origin),
            )
            .expect("return");
    }
    builder.finish()
}

#[test]
fn exact_task_list_carriers_transfer_through_construct_append_length_and_join() {
    validate_program(&carrier_program(CarrierCase::Append))
        .unwrap_or_else(|errors| panic!("valid carrier flow failed: {errors:#?}"));
}

#[test]
fn task_list_carrier_validation_rejects_aliases_reuse_cfg_duplication_and_get() {
    for case in [
        CarrierCase::DuplicateChild,
        CarrierCase::ReuseAfterJoin,
        CarrierCase::CfgAlias,
    ] {
        let Err(errors) = validate_program(&carrier_program(case)) else {
            panic!("invalid {case:?} carrier flow was accepted")
        };
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InvalidTaskOwnership
                && (error.message().contains("consumed more than once")
                    || error.message().contains("borrowed after it was consumed"))
        }));
    }

    let get = validate_program(&carrier_program(CarrierCase::ListGet))
        .expect_err("List.get cannot expose a child Task handle");
    assert!(get.as_slice().iter().any(|error| {
        error.code() == ValidationCode::InvalidTaskOwnership
            && error.message().contains("list.get cannot extract")
    }));

    let wrong = validate_program(&carrier_program(CarrierCase::WrongResult))
        .expect_err("dynamic all requires Task[List[T]]");
    assert!(
        wrong
            .as_slice()
            .iter()
            .any(|error| error.code() == ValidationCode::TypeMismatch)
    );
}

fn nested_task_output_program(indirect: bool) -> Program {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    builder
        .add_task_handle_type(Type::Task(Box::new(Type::Int)))
        .expect("Task[Int]");
    let task_list = Type::List(Box::new(Type::Task(Box::new(Type::Int))));
    builder
        .add_managed_list_type(task_list.clone())
        .expect("List[Task[Int]]");
    let output = if indirect {
        let wrapper = Type::Nominal(TypeId(90), Vec::new());
        builder
            .add_pod_record_type(wrapper.clone(), std::slice::from_ref(&task_list))
            .expect("raw wrapper");
        wrapper
    } else {
        task_list
    };
    builder
        .add_task_handle_type(Type::Task(Box::new(output)))
        .expect("raw nested Task output");
    builder.finish()
}

#[test]
fn representation_validation_rejects_direct_and_nominally_hidden_task_outputs() {
    for indirect in [false, true] {
        let errors = validate_program(&nested_task_output_program(indirect))
            .expect_err("Task outputs cannot carry nested Task ownership");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && (error.message().contains("task-free output")
                    || error.message().contains("non-Task direct values"))
        }));
    }
}
