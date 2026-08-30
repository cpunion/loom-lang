use std::collections::BTreeMap;

use loom_core::Span;
use loom_interpreter::{ContractFaultKind, ExecutionFailure, Interpreter, TestStatus, Value};
use loom_mir::{
    BinaryOp, Block, Builtin, CallArgument, CallPlan, CallTarget, CheckedProgram, ConceptDef,
    ConceptId, ConceptIdentity, Constant, ConstructionMode, Contract, ContractExpr,
    ContractExprKind, ContractValue, Expr, ExprId, ExprKind, FieldDef, Function, FunctionId,
    LocalDecl, LocalId, MirValidationCode, Place, PreludeIds, Program, Receiver, RequirementDef,
    RequirementId, RequirementType, ScopedDisposal, Statement, StatementKind, Type, TypeDef,
    TypeDefKind, TypeId, VariantDef, VariantId, Witness, WitnessId, WitnessRef,
};

fn checked(mut program: Program) -> CheckedProgram {
    program
        .renumber_expr_ids()
        .expect("renumber checked-MIR fixture expressions");
    program.into_checked().expect("valid checked-MIR fixture")
}

fn span() -> Span {
    Span::default()
}

fn local(id: u32, name: &str, ty: Type, mutable: bool) -> LocalDecl {
    LocalDecl {
        id: LocalId(id),
        name: name.into(),
        ty,
        mutable,
        span: span(),
    }
}

fn constant(value: Constant, ty: Type) -> Expr {
    Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Constant(value),
        ty,
        span: span(),
    }
}

fn copy(place: Place, ty: Type) -> Expr {
    Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Copy(place),
        ty,
        span: span(),
    }
}

#[allow(clippy::too_many_lines)]
fn scoped_cleanup_fault_program(outer_raw: i64, inner_raw: i64) -> CheckedProgram {
    let resource = Type::Nominal(TypeId(0), Vec::new());
    let resource_value = |raw| Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Record {
            ty: TypeId(0),
            type_arguments: Vec::new(),
            fields: vec![constant(Constant::Int(raw), Type::Int)],
            construction: ConstructionMode::Plain,
        },
        ty: resource.clone(),
        span: span(),
    };
    let scoped = |local, raw| Statement {
        kind: StatementKind::Scoped {
            local: LocalId(local),
            value: resource_value(raw),
            disposal: ScopedDisposal::StaticConcept {
                requirement: RequirementId(0),
                witness: WitnessRef::Concrete(WitnessId(0)),
                dispatch_type: resource.clone(),
            },
        },
        span: span(),
    };
    let raw = || {
        copy(
            Place {
                local: LocalId(0),
                projection: vec![0],
            },
            Type::Int,
        )
    };
    let equals = |value| Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Binary(
            BinaryOp::Equal,
            Box::new(raw()),
            Box::new(constant(Constant::Int(value), Type::Int)),
        ),
        ty: Type::Bool,
        span: span(),
    };
    let assertion_fault = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Block(Block {
            statements: vec![Statement {
                kind: StatementKind::Assert {
                    condition: constant(Constant::Bool(false), Type::Bool),
                },
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        }),
        ty: Type::Unit,
        span: span(),
    };
    let integer_fault = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Block(Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Binary(
                        BinaryOp::Divide,
                        Box::new(constant(Constant::Int(1), Type::Int)),
                        Box::new(constant(Constant::Int(0), Type::Int)),
                    ),
                    ty: Type::Int,
                    span: span(),
                }),
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        }),
        ty: Type::Unit,
        span: span(),
    };
    let dispose = Function {
        id: FunctionId(0),
        name: "std.resource.Resource.dispose".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "self", resource.clone(), true)],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: Some(Receiver::Mutable),
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::If {
                    condition: Box::new(equals(0)),
                    then_branch: Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                        span: span(),
                    },
                    else_branch: Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(Expr {
                            id: ExprId::UNASSIGNED,
                            kind: ExprKind::If {
                                condition: Box::new(equals(1)),
                                then_branch: Block {
                                    statements: Vec::new(),
                                    tail: Some(Box::new(assertion_fault)),
                                    span: span(),
                                },
                                else_branch: Block {
                                    statements: Vec::new(),
                                    tail: Some(Box::new(integer_fault)),
                                    span: span(),
                                },
                            },
                            ty: Type::Unit,
                            span: span(),
                        })),
                        span: span(),
                    },
                },
                ty: Type::Unit,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let main = Function {
        id: FunctionId(1),
        name: "main".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![
            local(0, "outer", resource.clone(), true),
            local(1, "inner", resource.clone(), true),
        ],
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![
                scoped(0, outer_raw),
                Statement {
                    kind: StatementKind::Evaluate(Expr {
                        id: ExprId::UNASSIGNED,
                        kind: ExprKind::Block(Block {
                            statements: vec![scoped(1, inner_raw)],
                            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                            span: span(),
                        }),
                        ty: Type::Unit,
                        span: span(),
                    }),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    checked(Program {
        types: vec![TypeDef {
            id: TypeId(0),
            name: "std.resource.Resource".into(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "raw".into(),
                    ty: Type::Int,
                    span: span(),
                }],
                invariant: None,
            },
        }],
        concepts: vec![
            ConceptDef {
                id: ConceptId(0),
                module: "std.resource".into(),
                name: "Dispose".into(),
                span: span(),
                identity: Some(ConceptIdentity::Dispose),
                dynamic: false,
                associated_types: Vec::new(),
                requirements: vec![RequirementId(0)],
            },
            ConceptDef {
                id: ConceptId(1),
                module: "std.resource".into(),
                name: "MustScope".into(),
                span: span(),
                identity: Some(ConceptIdentity::MustScope),
                dynamic: false,
                associated_types: Vec::new(),
                requirements: Vec::new(),
            },
            ConceptDef {
                id: ConceptId(2),
                module: "std.resource".into(),
                name: "NoSuspend".into(),
                span: span(),
                identity: Some(ConceptIdentity::NoSuspend),
                dynamic: false,
                associated_types: Vec::new(),
                requirements: Vec::new(),
            },
        ],
        requirements: vec![RequirementDef {
            id: RequirementId(0),
            concept: ConceptId(0),
            name: "dispose".into(),
            span: span(),
            receiver: Some(Receiver::Mutable),
            method_type_parameters: 0,
            params: vec![RequirementType::SelfType],
            return_ty: RequirementType::Unit,
            witness_params: Vec::new(),
        }],
        functions: vec![dispose, main],
        witnesses: vec![
            Witness {
                id: WitnessId(0),
                concept: ConceptId(0),
                concrete: resource.clone(),
                methods: BTreeMap::from([(RequirementId(0), FunctionId(0))]),
                associated: BTreeMap::new(),
                type_parameters: 0,
                prerequisites: Vec::new(),
            },
            Witness {
                id: WitnessId(1),
                concept: ConceptId(1),
                concrete: resource,
                methods: BTreeMap::new(),
                associated: BTreeMap::new(),
                type_parameters: 0,
                prerequisites: Vec::new(),
            },
        ],
        prelude: PreludeIds {
            dispose_concept: Some(ConceptId(0)),
            dispose_requirement: Some(RequirementId(0)),
            must_scope_concept: Some(ConceptId(1)),
            no_suspend_concept: Some(ConceptId(2)),
            ..PreludeIds::default()
        },
        ..Program::default()
    })
}

#[test]
fn scoped_cleanups_are_lexical_lifo_and_preserve_the_primary_fault() {
    let program = scoped_cleanup_fault_program(1, 0);
    let mut interpreter = Interpreter::new(&program);
    let failure = interpreter
        .invoke(FunctionId(1), Vec::new(), span())
        .expect_err("the outer lexical cleanup must still run after the inner cleanup");
    assert!(matches!(
        failure,
        ExecutionFailure::Contract { fault }
            if fault.code == "AssertionFault"
    ));

    let program = scoped_cleanup_fault_program(1, 2);
    let mut interpreter = Interpreter::new(&program);
    let failure = interpreter
        .invoke(FunctionId(1), Vec::new(), span())
        .expect_err("both lexical cleanups fault");
    assert!(
        matches!(
            &failure,
            ExecutionFailure::Runtime { fault }
                if fault.code == "IntegerDivisionByZero"
        ),
        "{failure:?}"
    );
}

#[test]
fn projected_move_consumes_the_root_and_returns_the_owned_leaf() {
    let pair = Type::Nominal(TypeId(0), Vec::new());
    let program = checked(Program {
        types: vec![TypeDef {
            id: TypeId(0),
            name: "Pair".into(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![
                    FieldDef {
                        name: "left".into(),
                        ty: Type::Int,
                        span: span(),
                    },
                    FieldDef {
                        name: "right".into(),
                        ty: Type::Int,
                        span: span(),
                    },
                ],
                invariant: None,
            },
        }],
        functions: vec![Function {
            id: FunctionId(0),
            name: "takeRight".into(),
            span: span(),
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: vec![local(0, "pair", pair, false)],
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: Vec::new(),
            return_ty: Type::Int,
            receiver: None,
            body: Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Move(Place {
                        local: LocalId(0),
                        projection: vec![1],
                    }),
                    ty: Type::Int,
                    span: span(),
                })),
                span: span(),
            },
            call_plan: CallPlan::default(),
        }],
        ..Program::default()
    });
    let value = Interpreter::new(&program)
        .invoke(
            FunctionId(0),
            vec![Value::Record {
                ty: TypeId(0),
                fields: vec![Value::Int { value: 1 }, Value::Int { value: 7 }],
            }],
            span(),
        )
        .expect("projected move executes");
    assert_eq!(value, Value::Int { value: 7 });
}

fn result_type() -> TypeDef {
    TypeDef {
        id: TypeId(0),
        name: "Result".into(),
        span: span(),
        type_parameters: 2,
        kind: TypeDefKind::Enum {
            variants: vec![
                VariantDef {
                    id: VariantId(0),
                    name: "Ok".into(),
                    payload: vec![Type::Parameter(0)],
                    span: span(),
                },
                VariantDef {
                    id: VariantId(1),
                    name: "Err".into(),
                    payload: vec![Type::Parameter(1)],
                    span: span(),
                },
            ],
        },
    }
}

fn constraint_error_type(id: TypeId) -> TypeDef {
    TypeDef {
        id,
        name: "ConstraintError".into(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![
                FieldDef {
                    name: "target_type".into(),
                    ty: Type::Text,
                    span: span(),
                },
                FieldDef {
                    name: "code".into(),
                    ty: Type::Text,
                    span: span(),
                },
                FieldDef {
                    name: "predicate".into(),
                    ty: Type::Text,
                    span: span(),
                },
                FieldDef {
                    name: "path".into(),
                    ty: Type::List(Box::new(Type::Text)),
                    span: span(),
                },
                FieldDef {
                    name: "value_summary".into(),
                    ty: Type::Text,
                    span: span(),
                },
                FieldDef {
                    name: "contract_span".into(),
                    ty: Type::Tuple(vec![Type::Int, Type::Int, Type::Int]),
                    span: span(),
                },
            ],
            invariant: None,
        },
    }
}

fn double_interface(dynamic: bool) -> (ConceptDef, RequirementDef) {
    (
        ConceptDef {
            id: ConceptId(0),
            module: "test".into(),
            name: "Double".into(),
            span: span(),
            identity: None,
            dynamic,
            associated_types: Vec::new(),
            requirements: vec![RequirementId(0)],
        },
        RequirementDef {
            id: RequirementId(0),
            concept: ConceptId(0),
            name: "read".into(),
            span: span(),
            receiver: Some(Receiver::Readonly),
            method_type_parameters: 0,
            params: vec![RequirementType::SelfType],
            return_ty: RequirementType::Int,
            witness_params: Vec::new(),
        },
    )
}

fn non_negative_contract() -> Contract {
    Contract {
        code: "non_negative".into(),
        span: span(),
        expression: ContractExpr {
            span: span(),
            kind: ContractExprKind::Binary(
                BinaryOp::GreaterEqual,
                Box::new(ContractExpr {
                    span: span(),
                    kind: ContractExprKind::Value(ContractValue::SelfValue),
                }),
                Box::new(ContractExpr {
                    span: span(),
                    kind: ContractExprKind::Constant(Constant::Float(0.0)),
                }),
            ),
        },
    }
}

fn rejected_contract(code: &str) -> Contract {
    Contract {
        code: code.into(),
        span: span(),
        expression: ContractExpr {
            span: span(),
            kind: ContractExprKind::Constant(Constant::Bool(false)),
        },
    }
}

fn unit_contract_function(
    id: u32,
    name: &str,
    is_async: bool,
    receiver: Option<Receiver>,
    call_plan: CallPlan,
) -> Function {
    let params = receiver
        .map(|_| local(0, "self", Type::Nominal(TypeId(0), Vec::new()), false))
        .into_iter()
        .collect();
    Function {
        id: FunctionId(id),
        name: name.into(),
        span: span(),
        type_parameters: 0,
        is_async,
        suspension_points: Vec::new(),
        params,
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
        call_plan,
    }
}

fn non_negative_first_field_contract() -> Contract {
    let mut contract = non_negative_contract();
    let ContractExprKind::Binary(_, left, _) = &mut contract.expression.kind else {
        unreachable!();
    };
    **left = ContractExpr {
        span: span(),
        kind: ContractExprKind::Field(
            Box::new(ContractExpr {
                span: span(),
                kind: ContractExprKind::Value(ContractValue::SelfValue),
            }),
            0,
        ),
    };
    contract
}

#[test]
fn refined_construction_returns_language_result() {
    let price = TypeDef {
        id: TypeId(2),
        name: "shop.Price".into(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Refined {
            base: Type::Float,
            predicate: non_negative_contract(),
        },
    };
    let function = Function {
        id: FunctionId(0),
        name: "make_price".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "raw", Type::Float, false)],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Nominal(
            TypeId(0),
            vec![
                Type::Nominal(TypeId(2), Vec::new()),
                Type::Nominal(TypeId(1), Vec::new()),
            ],
        ),
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Refine {
                    ty: TypeId(2),
                    value: Box::new(copy(Place::local(LocalId(0)), Type::Float)),
                    construction: ConstructionMode::Runtime,
                },
                ty: Type::Nominal(
                    TypeId(0),
                    vec![
                        Type::Nominal(TypeId(2), Vec::new()),
                        Type::Nominal(TypeId(1), Vec::new()),
                    ],
                ),
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let program = Program {
        types: vec![result_type(), constraint_error_type(TypeId(1)), price],
        functions: vec![function],
        prelude: PreludeIds {
            result: Some(TypeId(0)),
            constraint_error: Some(TypeId(1)),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    let program = checked(program);
    let mut interpreter = Interpreter::new(&program);

    let accepted = interpreter
        .invoke(FunctionId(0), vec![Value::Float { value: 12.5 }], span())
        .expect("accepted price");
    assert!(matches!(
        accepted,
        Value::Enum {
            variant: VariantId(0),
            ..
        }
    ));

    let rejected = interpreter
        .invoke(
            FunctionId(0),
            vec![Value::Float { value: -97_531.125 }],
            span(),
        )
        .expect("constraint_error is data, not a runtime failure");
    let Value::Enum {
        variant: VariantId(1),
        payload,
        ..
    } = rejected
    else {
        panic!("rejected construction must return Err");
    };
    let [Value::ConstraintError { value }] = payload.as_slice() else {
        panic!("Err must contain ConstraintError");
    };
    assert_eq!(value.target_type, "shop.Price");
    assert_eq!(value.code, "ConstraintViolation");
    assert_eq!(value.predicate, "non_negative");
    assert!(value.path.is_empty());
    assert_eq!(value.value_summary, "Float");
    assert!(!value.value_summary.contains("97531"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn mutable_receiver_is_an_inout_place_and_exit_invariant_wins() {
    let order = TypeDef {
        id: TypeId(0),
        name: "shop.Order".into(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "total".into(),
                ty: Type::Float,
                span: span(),
            }],
            invariant: Some(non_negative_first_field_contract()),
        },
    };
    let set_total = Function {
        id: FunctionId(0),
        name: "shop.Order.set_total".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![
            local(0, "self", Type::Nominal(TypeId(0), vec![]), true),
            local(1, "value", Type::Float, false),
        ],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: Some(Receiver::Mutable),
        body: Block {
            statements: vec![Statement {
                kind: StatementKind::Assign {
                    place: Place {
                        local: LocalId(0),
                        projection: vec![0],
                    },
                    value: copy(Place::local(LocalId(1)), Type::Float),
                },
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
        call_plan: CallPlan {
            receiver_invariant: Some(non_negative_first_field_contract()),
            ..CallPlan::default()
        },
    };
    let wrapper = Function {
        id: FunctionId(1),
        name: "set_and_read".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![
            local(0, "order", Type::Nominal(TypeId(0), vec![]), true),
            local(1, "value", Type::Float, false),
        ],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![local(2, "ignored", Type::Unit, false)],
        return_ty: Type::Float,
        receiver: None,
        body: Block {
            statements: vec![Statement {
                kind: StatementKind::Let {
                    local: LocalId(2),
                    value: Expr {
                        id: ExprId::UNASSIGNED,
                        kind: ExprKind::Call {
                            target: CallTarget::Inherent(FunctionId(0)),
                            type_arguments: Vec::new(),
                            arguments: vec![
                                CallArgument::InOut(Place::local(LocalId(0))),
                                CallArgument::Value(copy(Place::local(LocalId(1)), Type::Float)),
                            ],
                            witnesses: Vec::new(),
                        },
                        ty: Type::Unit,
                        span: span(),
                    },
                },
                span: span(),
            }],
            tail: Some(Box::new(copy(
                Place {
                    local: LocalId(0),
                    projection: vec![0],
                },
                Type::Float,
            ))),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let program = Program {
        types: vec![order],
        functions: vec![set_total, wrapper],
        ..Program::default()
    };
    let initial = Value::Record {
        ty: TypeId(0),
        fields: vec![Value::Float { value: 10.0 }],
    };

    let program = checked(program);
    let mut interpreter = Interpreter::new(&program);
    let value = interpreter
        .invoke(
            FunctionId(1),
            vec![initial.clone(), Value::Float { value: 8.0 }],
            span(),
        )
        .expect("valid mutation");
    assert_eq!(value, Value::Float { value: 8.0 });

    let failure = interpreter
        .invoke(
            FunctionId(1),
            vec![initial, Value::Float { value: -1.0 }],
            span(),
        )
        .expect_err("exit invariant must fail");
    assert!(matches!(
        failure,
        ExecutionFailure::Contract { fault }
            if fault.category == ContractFaultKind::Invariant
    ));
}

#[test]
fn early_return_propagates_through_nested_control_expression() {
    let function = Function {
        id: FunctionId(0),
        name: "nested_return".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::If {
                    condition: Box::new(constant(Constant::Bool(true), Type::Bool)),
                    then_branch: Block {
                        statements: vec![Statement {
                            kind: StatementKind::Return(Some(constant(
                                Constant::Int(7),
                                Type::Int,
                            ))),
                            span: span(),
                        }],
                        tail: None,
                        span: span(),
                    },
                    else_branch: Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(constant(Constant::Int(0), Type::Int))),
                        span: span(),
                    },
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let program = Program {
        functions: vec![function],
        ..Program::default()
    };
    let program = checked(program);
    let mut interpreter = Interpreter::new(&program);
    assert_eq!(
        interpreter
            .invoke(FunctionId(0), Vec::new(), span())
            .expect("return"),
        Value::Int { value: 7 }
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn dyn_view_dispatches_only_through_its_embedded_witness() {
    let number = TypeDef {
        id: TypeId(0),
        name: "NumberBox".into(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "value".into(),
                ty: Type::Int,
                span: span(),
            }],
            invariant: None,
        },
    };
    let method = Function {
        id: FunctionId(0),
        name: "Double.read".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "self", Type::Nominal(TypeId(0), vec![]), false)],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: Some(Receiver::Readonly),
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Binary(
                    BinaryOp::Multiply,
                    Box::new(copy(
                        Place {
                            local: LocalId(0),
                            projection: vec![0],
                        },
                        Type::Int,
                    )),
                    Box::new(constant(Constant::Int(2), Type::Int)),
                ),
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let view_ty = Type::View {
        mutable: false,
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
    };
    let wrapper = Function {
        id: FunctionId(1),
        name: "read_dynamic".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "number", Type::Nominal(TypeId(0), vec![]), false)],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![local(1, "reader", view_ty.clone(), false)],
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: vec![Statement {
                kind: StatementKind::Let {
                    local: LocalId(1),
                    value: Expr {
                        id: ExprId::UNASSIGNED,
                        kind: ExprKind::MakeView {
                            value: Box::new(copy(
                                Place::local(LocalId(0)),
                                Type::Nominal(TypeId(0), vec![]),
                            )),
                            writeback: None,
                            witness: WitnessRef::Concrete(WitnessId(0)),
                            mutable: false,
                            token: 0,
                        },
                        ty: view_ty.clone(),
                        span: span(),
                    },
                },
                span: span(),
            }],
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Call {
                    target: CallTarget::Dynamic {
                        requirement: RequirementId(0),
                    },
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(copy(Place::local(LocalId(1)), view_ty))],
                    witnesses: Vec::new(),
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let witness = Witness {
        id: WitnessId(0),
        concept: ConceptId(0),
        concrete: Type::Nominal(TypeId(0), Vec::new()),
        methods: BTreeMap::from([(RequirementId(0), FunctionId(0))]),
        associated: BTreeMap::new(),
        type_parameters: 0,
        prerequisites: Vec::new(),
    };
    let (concept, requirement) = double_interface(true);
    let program = Program {
        types: vec![number],
        concepts: vec![concept],
        requirements: vec![requirement],
        functions: vec![method, wrapper],
        witnesses: vec![witness],
        ..Program::default()
    };
    let program = checked(program);
    let mut interpreter = Interpreter::new(&program);
    let value = interpreter
        .invoke(
            FunctionId(1),
            vec![Value::Record {
                ty: TypeId(0),
                fields: vec![Value::Int { value: 21 }],
            }],
            span(),
        )
        .expect("dynamic dispatch");
    assert_eq!(value, Value::Int { value: 42 });
}

#[test]
fn test_runner_distinguishes_ok_and_err_values() {
    let pass = Function {
        id: FunctionId(0),
        name: "pass".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let program = Program {
        functions: vec![pass],
        tests: vec![FunctionId(0)],
        ..Program::default()
    };
    let program = checked(program);
    let mut interpreter = Interpreter::new(&program);
    let results = interpreter.run_tests();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, TestStatus::Passed);
}

#[test]
#[allow(clippy::too_many_lines)]
fn generic_static_concept_call_forwards_hidden_witness_parameter() {
    let number = TypeDef {
        id: TypeId(0),
        name: "NumberBox".into(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "value".into(),
                ty: Type::Int,
                span: span(),
            }],
            invariant: None,
        },
    };
    let method = Function {
        id: FunctionId(0),
        name: "Double.read".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "self", Type::Nominal(TypeId(0), vec![]), false)],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: Some(Receiver::Readonly),
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Binary(
                    BinaryOp::Multiply,
                    Box::new(copy(
                        Place {
                            local: LocalId(0),
                            projection: vec![0],
                        },
                        Type::Int,
                    )),
                    Box::new(constant(Constant::Int(2), Type::Int)),
                ),
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let generic = Function {
        id: FunctionId(1),
        name: "read_twice[T: Double]".into(),
        span: span(),
        type_parameters: 1,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "value", Type::Parameter(0), false)],
        witness_params: vec![loom_mir::WitnessParam {
            target: Type::Parameter(0),
            concept: ConceptId(0),
            bindings: BTreeMap::new(),
            span: span(),
        }],
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Call {
                    target: CallTarget::StaticConcept {
                        requirement: RequirementId(0),
                        witness: WitnessRef::Parameter(0),
                        dispatch_type: Type::Parameter(0),
                    },
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(copy(
                        Place::local(LocalId(0)),
                        Type::Parameter(0),
                    ))],
                    witnesses: Vec::new(),
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let wrapper = Function {
        id: FunctionId(2),
        name: "read_number".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "number", Type::Nominal(TypeId(0), vec![]), false)],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(1)),
                    type_arguments: vec![Type::Nominal(TypeId(0), Vec::new())],
                    arguments: vec![CallArgument::Value(copy(
                        Place::local(LocalId(0)),
                        Type::Nominal(TypeId(0), vec![]),
                    ))],
                    witnesses: vec![WitnessRef::Concrete(WitnessId(0))],
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let witness = Witness {
        id: WitnessId(0),
        concept: ConceptId(0),
        concrete: Type::Nominal(TypeId(0), Vec::new()),
        methods: BTreeMap::from([(RequirementId(0), FunctionId(0))]),
        associated: BTreeMap::new(),
        type_parameters: 0,
        prerequisites: Vec::new(),
    };
    let (concept, requirement) = double_interface(false);
    let program = Program {
        types: vec![number],
        concepts: vec![concept],
        requirements: vec![requirement],
        functions: vec![method, generic, wrapper],
        witnesses: vec![witness],
        ..Program::default()
    };
    let program = checked(program);
    let mut interpreter = Interpreter::new(&program);
    assert_eq!(
        interpreter
            .invoke(
                FunctionId(2),
                vec![Value::Record {
                    ty: TypeId(0),
                    fields: vec![Value::Int { value: 21 }],
                }],
                span(),
            )
            .expect("generic witness dispatch"),
        Value::Int { value: 42 }
    );
}

#[test]
fn integer_overflow_is_a_runtime_fault_and_float_equality_is_ieee() {
    let overflow = Function {
        id: FunctionId(0),
        name: "overflow".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Binary(
                    BinaryOp::Add,
                    Box::new(constant(Constant::Int(i64::MAX), Type::Int)),
                    Box::new(constant(Constant::Int(1), Type::Int)),
                ),
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let nan_equal = Function {
        id: FunctionId(1),
        name: "nan_equal".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Bool,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Binary(
                    BinaryOp::Equal,
                    Box::new(constant(Constant::Float(f64::NAN), Type::Float)),
                    Box::new(constant(Constant::Float(f64::NAN), Type::Float)),
                ),
                ty: Type::Bool,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let program = Program {
        functions: vec![overflow, nan_equal],
        ..Program::default()
    };
    let program = checked(program);
    let mut interpreter = Interpreter::new(&program);
    let failure = interpreter
        .invoke(FunctionId(0), Vec::new(), span())
        .expect_err("checked i64 overflow must stop the execution channel");
    assert!(matches!(
        failure,
        ExecutionFailure::Runtime { fault }
            if fault.code == "IntegerOverflow"
    ));
    assert_eq!(
        interpreter
            .invoke(FunctionId(1), Vec::new(), span())
            .expect("IEEE comparison"),
        Value::Bool { value: false }
    );
}

#[test]
fn invalid_program_cannot_cross_the_interpreter_boundary() {
    let program = Program {
        tests: vec![FunctionId(99)],
        ..Program::default()
    };
    let errors = program
        .into_checked()
        .expect_err("an invalid test root must not cross checked MIR");
    assert!(errors.contains(MirValidationCode::InvalidFunctionReference));
}

#[test]
fn value_summaries_are_type_only_and_do_not_disclose_secrets_or_sizes() {
    let cases = [
        (Value::Bool { value: true }, "Bool"),
        (Value::Int { value: 97_531 }, "Int"),
        (Value::Float { value: 97_531.125 }, "Float"),
        (
            Value::Text {
                value: "customer-secret".into(),
            },
            "Text",
        ),
        (
            Value::Bytes {
                value: b"customer-secret".to_vec(),
            },
            "Bytes",
        ),
        (
            Value::List {
                elements: vec![Value::Text {
                    value: "customer-secret".into(),
                }],
            },
            "List",
        ),
    ];
    for (value, expected) in cases {
        let summary = value.summary();
        assert_eq!(summary, expected);
        assert!(!summary.contains("customer-secret"));
        assert!(!summary.contains("15"));
        assert!(!summary.contains("97531"));
    }
}

#[test]
fn method_contract_arguments_exclude_the_receiver() {
    let requires_seven = Contract {
        code: "argument_is_seven".into(),
        span: span(),
        expression: ContractExpr {
            span: span(),
            kind: ContractExprKind::Binary(
                BinaryOp::Equal,
                Box::new(ContractExpr {
                    span: span(),
                    kind: ContractExprKind::Value(ContractValue::Argument(0)),
                }),
                Box::new(ContractExpr {
                    span: span(),
                    kind: ContractExprKind::Constant(Constant::Int(7)),
                }),
            ),
        },
    };
    let method = Function {
        id: FunctionId(0),
        name: "sample.R.accept".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![
            local(0, "self", Type::Nominal(TypeId(0), Vec::new()), false),
            local(1, "value", Type::Int, false),
        ],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: Some(Receiver::Readonly),
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(copy(Place::local(LocalId(1)), Type::Int))),
            span: span(),
        },
        call_plan: CallPlan {
            requires: vec![requires_seven],
            ..CallPlan::default()
        },
    };
    let program = Program {
        types: vec![TypeDef {
            id: TypeId(0),
            name: "sample.R".into(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: Vec::new(),
                invariant: None,
            },
        }],
        functions: vec![method],
        ..Program::default()
    };
    let receiver = Value::Record {
        ty: TypeId(0),
        fields: Vec::new(),
    };
    let program = checked(program);
    let mut interpreter = Interpreter::new(&program);
    assert_eq!(
        interpreter
            .invoke(
                FunctionId(0),
                vec![receiver.clone(), Value::Int { value: 7 }],
                span(),
            )
            .expect("first declared value parameter is contract argument zero"),
        Value::Int { value: 7 }
    );
    let failure = interpreter
        .invoke(
            FunctionId(0),
            vec![receiver, Value::Int { value: 6 }],
            span(),
        )
        .expect_err("precondition must reject the other value");
    assert!(matches!(
        failure,
        ExecutionFailure::Contract { fault }
            if fault.code == "PreconditionFault"
    ));
}

#[test]
fn entry_contract_order_distinguishes_sync_functions_methods_and_async_methods() {
    let record_invariant = rejected_contract("record.invariant");
    let ordinary = unit_contract_function(
        0,
        "ordinary",
        false,
        None,
        CallPlan {
            requires: vec![
                rejected_contract("ordinary.first"),
                rejected_contract("ordinary.second"),
            ],
            ..CallPlan::default()
        },
    );
    let sync_method = unit_contract_function(
        1,
        "sample.R.sync_method",
        false,
        Some(Receiver::Readonly),
        CallPlan {
            receiver_invariant: Some(record_invariant.clone()),
            requires: vec![rejected_contract("sync.method.requires")],
            ..CallPlan::default()
        },
    );
    let async_method = unit_contract_function(
        2,
        "sample.R.async_method",
        true,
        Some(Receiver::Readonly),
        CallPlan {
            receiver_invariant: Some(record_invariant.clone()),
            requires: vec![rejected_contract("async.method.requires")],
            ..CallPlan::default()
        },
    );
    let program = checked(Program {
        types: vec![TypeDef {
            id: TypeId(0),
            name: "sample.R".into(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: Vec::new(),
                invariant: Some(record_invariant),
            },
        }],
        functions: vec![ordinary, sync_method, async_method],
        ..Program::default()
    });
    let mut interpreter = Interpreter::new(&program);
    let receiver = || Value::Record {
        ty: TypeId(0),
        fields: Vec::new(),
    };

    let failure = interpreter
        .invoke(FunctionId(0), Vec::new(), span())
        .expect_err("the first declared precondition must fail");
    let ExecutionFailure::Contract { fault } = failure else {
        panic!("ordinary precondition must produce a contract fault");
    };
    assert_eq!(fault.category, ContractFaultKind::Precondition);
    assert!(fault.message.contains("ordinary.first"), "{fault:?}");

    let failure = interpreter
        .invoke(FunctionId(1), vec![receiver()], span())
        .expect_err("the synchronous precondition must precede the entry invariant");
    let ExecutionFailure::Contract { fault } = failure else {
        panic!("method precondition must produce a contract fault");
    };
    assert_eq!(fault.category, ContractFaultKind::Precondition);
    assert_eq!(fault.code, "PreconditionFault");
    assert!(fault.message.contains("sync.method.requires"), "{fault:?}");

    let failure = interpreter
        .invoke(FunctionId(2), vec![receiver()], span())
        .expect_err("the async entry invariant must remain first");
    let ExecutionFailure::Contract { fault } = failure else {
        panic!("async invariant must produce a contract fault");
    };
    assert_eq!(fault.category, ContractFaultKind::Invariant);
    assert_eq!(fault.code, "InvariantFault");
    assert!(fault.message.contains("record.invariant"), "{fault:?}");
}

#[test]
fn projecting_a_refined_record_reaches_the_requested_field() {
    let record = TypeDef {
        id: TypeId(0),
        name: "Record".into(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "value".into(),
                ty: Type::Int,
                span: span(),
            }],
            invariant: None,
        },
    };
    let refined = TypeDef {
        id: TypeId(1),
        name: "RefinedRecord".into(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Refined {
            base: Type::Nominal(TypeId(0), Vec::new()),
            predicate: Contract {
                code: "record_is_valid".into(),
                span: span(),
                expression: ContractExpr {
                    span: span(),
                    kind: ContractExprKind::Constant(Constant::Bool(true)),
                },
            },
        },
    };
    let function = Function {
        id: FunctionId(0),
        name: "read".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(
            0,
            "value",
            Type::Nominal(TypeId(1), Vec::new()),
            false,
        )],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(copy(
                Place {
                    local: LocalId(0),
                    projection: vec![0],
                },
                Type::Int,
            ))),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let program = Program {
        types: vec![record, refined],
        functions: vec![function],
        ..Program::default()
    };
    let value = Value::Refined {
        ty: TypeId(1),
        value: Box::new(Value::Record {
            ty: TypeId(0),
            fields: vec![Value::Int { value: 42 }],
        }),
    };
    let program = checked(program);
    assert_eq!(
        Interpreter::new(&program)
            .invoke(FunctionId(0), vec![value], span())
            .expect("projection through a refined base record"),
        Value::Int { value: 42 }
    );
}

#[test]
#[allow(clippy::too_many_lines, clippy::float_cmp)]
fn float_text_builtins_follow_the_frozen_boundary() {
    let parse_result_ty = Type::Tuple(vec![Type::Float, Type::Int]);
    let parse = Function {
        id: FunctionId(0),
        name: "parse".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "text", Type::Text, false)],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: parse_result_ty.clone(),
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Call {
                    target: CallTarget::Builtin(Builtin::FloatParseStatus),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(copy(
                        Place::local(LocalId(0)),
                        Type::Text,
                    ))],
                    witnesses: Vec::new(),
                },
                ty: parse_result_ty,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let format = Function {
        id: FunctionId(1),
        name: "format".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "value", Type::Float, false)],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Text,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Call {
                    target: CallTarget::Builtin(Builtin::FloatFormat),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(copy(
                        Place::local(LocalId(0)),
                        Type::Float,
                    ))],
                    witnesses: Vec::new(),
                },
                ty: Type::Text,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let program = Program {
        types: Vec::new(),
        functions: vec![parse, format],
        ..Program::default()
    };
    let program = checked(program);
    let mut interpreter = Interpreter::new(&program);

    let valid = interpreter
        .invoke(
            FunctionId(0),
            vec![Value::Text {
                value: "1e3".into(),
            }],
            span(),
        )
        .expect("valid float");
    assert!(matches!(
        valid,
        Value::Tuple { elements }
            if matches!(elements.as_slice(), [Value::Float { value }, Value::Int { value: 0 }] if *value == 1000.0)
    ));

    for (text, expected_status) in [("1", 1), ("1e999", 2), ("inf", 1)] {
        let rejected = interpreter
            .invoke(
                FunctionId(0),
                vec![Value::Text { value: text.into() }],
                span(),
            )
            .expect("parse failure is a status value");
        assert!(matches!(
            rejected,
            Value::Tuple { elements }
                if matches!(elements.as_slice(), [Value::Float { value: 0.0 }, Value::Int { value }] if *value == expected_status)
        ));
    }

    for (value, expected) in [
        (1.0, "1.0"),
        (-0.0, "-0.0"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
        (f64::NAN, "NaN"),
    ] {
        assert_eq!(
            interpreter
                .invoke(FunctionId(1), vec![Value::Float { value }], span())
                .expect("format"),
            Value::Text {
                value: expected.into()
            }
        );
    }
}

#[test]
fn async_tasks_resume_through_the_ready_queue_and_collect_frames() {
    let child = Function {
        id: FunctionId(0),
        name: "child".into(),
        span: span(),
        type_parameters: 0,
        is_async: true,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Int(7), Type::Int))),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let task = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Call {
            target: CallTarget::Direct(FunctionId(0)),
            type_arguments: Vec::new(),
            arguments: Vec::new(),
            witnesses: Vec::new(),
        },
        ty: Type::Task(Box::new(Type::Int)),
        span: span(),
    };
    let parent = Function {
        id: FunctionId(1),
        name: "parent".into(),
        span: span(),
        type_parameters: 0,
        is_async: true,
        suspension_points: vec![loom_mir::SuspensionPoint {
            state: 1,
            span: span(),
            live_locals: Vec::new(),
        }],
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Await {
                    state: 1,
                    task: Box::new(task),
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let mut program = Program {
        functions: vec![child, parent],
        ..Program::default()
    };
    program
        .renumber_expr_ids()
        .expect("async test expression ids");
    let program = program.into_checked().expect("async MIR validates");

    let mut interpreter = Interpreter::new(&program);
    let value = interpreter
        .invoke(FunctionId(1), Vec::new(), span())
        .expect("async parent completes");
    assert_eq!(value, Value::Int { value: 7 });
    let stats = interpreter.gc_stats();
    assert_eq!(stats.allocations, 2);
    assert_eq!(stats.reclaimed, 2);
    assert_eq!(stats.live, 0);
    assert!(stats.collections >= 2);
}

#[test]
fn async_entry_and_exit_contracts_fault_the_task() {
    for (phase, expected) in [
        ("requires", ContractFaultKind::Precondition),
        ("ensures", ContractFaultKind::Postcondition),
    ] {
        let rejected = Contract {
            code: format!("async_{phase}"),
            span: span(),
            expression: ContractExpr {
                kind: ContractExprKind::Constant(Constant::Bool(false)),
                span: span(),
            },
        };
        let mut call_plan = CallPlan::default();
        if phase == "requires" {
            call_plan.requires.push(rejected);
        } else {
            call_plan.ensures.push(rejected);
        }
        let function = Function {
            id: FunctionId(0),
            name: format!("async_{phase}"),
            span: span(),
            type_parameters: 0,
            is_async: true,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: Vec::new(),
            return_ty: Type::Unit,
            receiver: None,
            body: Block {
                statements: Vec::new(),
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
            call_plan,
        };
        let mut program = Program {
            functions: vec![function],
            ..Program::default()
        };
        program
            .renumber_expr_ids()
            .expect("async contract test expression ids");
        let program = program
            .into_checked()
            .expect("async contract MIR validates");
        let failure = Interpreter::new(&program)
            .invoke(FunctionId(0), Vec::new(), span())
            .expect_err("rejected async contract faults its task");
        assert!(
            matches!(&failure, ExecutionFailure::Contract { fault } if fault.category == expected),
            "{phase}: {failure:?}"
        );
    }
}
