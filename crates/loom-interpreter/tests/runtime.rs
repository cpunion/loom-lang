use std::collections::BTreeMap;

use loom_core::Span;
use loom_interpreter::{ContractFaultKind, ExecutionFailure, Interpreter, TestStatus, Value};
use loom_mir::{
    BinaryOp, Block, Builtin, CallArgument, CallPlan, CallTarget, ConceptId, Constant, Contract,
    ContractExpr, ContractExprKind, ContractValue, Expr, ExprKind, FieldDef, Function, FunctionId,
    LocalDecl, LocalId, Place, PreludeIds, Program, Receiver, RequirementId, Statement,
    StatementKind, Type, TypeDef, TypeDefKind, TypeId, VariantDef, VariantId, Witness, WitnessId,
    WitnessRef,
};

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
        kind: ExprKind::Constant(value),
        ty,
        span: span(),
    }
}

fn copy(place: Place, ty: Type) -> Expr {
    Expr {
        kind: ExprKind::Copy(place),
        ty,
        span: span(),
    }
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

fn parse_float_error_type() -> TypeDef {
    TypeDef {
        id: TypeId(1),
        name: "standard.float.ParseFloatError".into(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Enum {
            variants: vec![
                VariantDef {
                    id: VariantId(0),
                    name: "InvalidSyntax".into(),
                    payload: Vec::new(),
                    span: span(),
                },
                VariantDef {
                    id: VariantId(1),
                    name: "OutOfRange".into(),
                    payload: Vec::new(),
                    span: span(),
                },
            ],
        },
    }
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
        id: TypeId(1),
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
        locals: Vec::new(),
        return_ty: Type::Nominal(TypeId(0), vec![Type::Nominal(TypeId(1), vec![])]),
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                kind: ExprKind::Refine {
                    ty: TypeId(1),
                    value: Box::new(copy(Place::local(LocalId(0)), Type::Float)),
                },
                ty: Type::Nominal(TypeId(0), Vec::new()),
                span: span(),
            })),
            span: span(),
        },
        call_plan: CallPlan::default(),
    };
    let program = Program {
        types: vec![result_type(), price],
        functions: vec![function],
        prelude: PreludeIds {
            result: Some(TypeId(0)),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
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
        .invoke(FunctionId(0), vec![Value::Float { value: -0.01 }], span())
        .expect("violation is data, not a runtime failure");
    assert!(matches!(
        rejected,
        Value::Enum {
            variant: VariantId(1),
            payload,
            ..
        } if matches!(payload.as_slice(), [Value::Violation { .. }])
    ));
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
        locals: vec![local(2, "ignored", Type::Unit, false)],
        return_ty: Type::Float,
        receiver: None,
        body: Block {
            statements: vec![Statement {
                kind: StatementKind::Let {
                    local: LocalId(2),
                    value: Expr {
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
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
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
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: Some(Receiver::Readonly),
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
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
        locals: vec![local(1, "reader", view_ty.clone(), false)],
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: vec![Statement {
                kind: StatementKind::Let {
                    local: LocalId(1),
                    value: Expr {
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
    let program = Program {
        types: vec![number],
        functions: vec![method, wrapper],
        witnesses: vec![witness],
        ..Program::default()
    };
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
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: Some(Receiver::Readonly),
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
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
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
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
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
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
    let program = Program {
        types: vec![number],
        functions: vec![method, generic, wrapper],
        witnesses: vec![witness],
        ..Program::default()
    };
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
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
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
        locals: Vec::new(),
        return_ty: Type::Bool,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
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
fn invalid_checked_boundary_state_is_reported_as_an_interpreter_defect() {
    let program = Program {
        tests: vec![FunctionId(99)],
        ..Program::default()
    };
    let results = Interpreter::new(&program).run_tests();
    let [result] = results.as_slice() else {
        panic!("one invalid test reference must still produce one report");
    };
    assert!(matches!(
        result.failure,
        Some(ExecutionFailure::Defect { ref defect })
            if defect.code == "InterpreterDefect"
    ));
}

#[test]
fn value_summaries_do_not_disclose_text_contents() {
    let value = Value::Text {
        value: "customer-secret".into(),
    };
    assert_eq!(value.summary(), "Text(bytes=15)");
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
fn projecting_a_refined_record_reaches_the_requested_field() {
    let function = Function {
        id: FunctionId(0),
        name: "read".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "value", Type::Error, false)],
        witness_params: Vec::new(),
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
    let parse = Function {
        id: FunctionId(0),
        name: "parse".into(),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![local(0, "text", Type::Text, false)],
        witness_params: Vec::new(),
        locals: Vec::new(),
        return_ty: Type::Error,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                kind: ExprKind::Call {
                    target: CallTarget::Builtin(Builtin::ParseFloat),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(copy(
                        Place::local(LocalId(0)),
                        Type::Text,
                    ))],
                    witnesses: Vec::new(),
                },
                ty: Type::Error,
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
        locals: Vec::new(),
        return_ty: Type::Text,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                kind: ExprKind::Call {
                    target: CallTarget::Builtin(Builtin::FormatFloat),
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
        types: vec![result_type(), parse_float_error_type()],
        functions: vec![parse, format],
        prelude: PreludeIds {
            result: Some(TypeId(0)),
            parse_float_error: Some(TypeId(1)),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
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
        Value::Enum { variant: VariantId(0), payload, .. }
            if matches!(payload.as_slice(), [Value::Float { value }] if *value == 1000.0)
    ));

    for (text, expected_variant) in [("1", 0), ("1e999", 1), ("inf", 0)] {
        let rejected = interpreter
            .invoke(
                FunctionId(0),
                vec![Value::Text { value: text.into() }],
                span(),
            )
            .expect("parse failure is a Result.Err value");
        assert!(matches!(
            rejected,
            Value::Enum { variant: VariantId(1), payload, .. }
                if matches!(payload.as_slice(), [Value::Enum { variant, .. }] if variant.0 == expected_variant)
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
        locals: Vec::new(),
        return_ty: Type::Int,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
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
    let program = Program {
        functions: vec![child, parent],
        ..Program::default()
    };
    program.validate().expect("async MIR validates");

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
