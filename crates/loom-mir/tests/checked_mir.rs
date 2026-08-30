use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use loom_core::Span;
use loom_mir::{
    ArtifactError, AssociatedTypeDef, BinaryOp, Block, CallArgument, CallPlan, CallTarget,
    CheckedProgram, ConceptDef, ConceptId, ConceptIdentity, Constant, ConstructionMode, Contract,
    ContractArm, ContractExpr, ContractExprKind, ContractValue, Expr, ExprId, ExprKind, FieldDef,
    Function, FunctionId, INTERPRETED_ARTIFACT_VERSION, LocalDecl, LocalId, MatchArm,
    MirValidationCode, Pattern, Place, PreludeIds, Program, Receiver, RequirementDef,
    RequirementId, RequirementType, RequirementWitnessParam, ScopedDisposal, Statement,
    StatementKind, SuspensionPoint, Type, TypeDef, TypeDefKind, TypeId, VariantDef, VariantId,
    Witness, WitnessId, WitnessParam, WitnessRef, decode_interpreted_artifact,
    decode_interpreted_executable_artifact, encode_interpreted_artifact,
    encode_interpreted_executable_artifact, validate_program,
};
use wait_timeout::ChildExt as _;

fn span() -> Span {
    Span::default()
}

fn local(id: u32, ty: Type, mutable: bool) -> LocalDecl {
    LocalDecl {
        id: LocalId(id),
        name: format!("local_{id}"),
        ty,
        mutable,
        span: span(),
    }
}

fn expr(kind: ExprKind, ty: Type) -> Expr {
    Expr {
        id: ExprId::UNASSIGNED,
        kind,
        ty,
        span: span(),
    }
}

fn constant(value: Constant, ty: Type) -> Expr {
    expr(ExprKind::Constant(value), ty)
}

fn constraint_error_fields() -> Vec<FieldDef> {
    [
        ("target_type", Type::Text),
        ("code", Type::Text),
        ("predicate", Type::Text),
        ("path", Type::List(Box::new(Type::Text))),
        ("value_summary", Type::Text),
        (
            "contract_span",
            Type::Tuple(vec![Type::Int, Type::Int, Type::Int]),
        ),
    ]
    .into_iter()
    .map(|(name, ty)| FieldDef {
        name: name.to_owned(),
        ty,
        span: span(),
    })
    .collect()
}

fn copy(id: u32, ty: Type) -> Expr {
    expr(ExprKind::Copy(Place::local(LocalId(id))), ty)
}

fn move_local(id: u32, ty: Type) -> Expr {
    expr(ExprKind::Move(Place::local(LocalId(id))), ty)
}

fn copy_place(place: Place, ty: Type) -> Expr {
    expr(ExprKind::Copy(place), ty)
}

fn function(
    id: u32,
    params: Vec<LocalDecl>,
    locals: Vec<LocalDecl>,
    return_ty: Type,
    body: Block,
) -> Function {
    let mut function = Function {
        id: FunctionId(id),
        name: format!("function_{id}"),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params,
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals,
        return_ty,
        receiver: None,
        body,
        call_plan: CallPlan::default(),
    };
    function
        .renumber_expr_ids()
        .expect("test function expression ids");
    function
}

fn simple_program() -> Program {
    Program {
        functions: vec![function(
            0,
            vec![local(0, Type::Int, false)],
            vec![local(1, Type::Int, true)],
            Type::Int,
            Block {
                statements: vec![Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(7), Type::Int),
                    },
                    span: span(),
                }],
                tail: Some(Box::new(copy(1, Type::Int))),
                span: span(),
            },
        )],
        ..Program::default()
    }
}

fn validation_errors(program: &Program) -> loom_mir::MirValidationErrors {
    validate_program(program).expect_err("program should be invalid")
}

fn sleep_await(state: u32) -> Expr {
    expr(
        ExprKind::Await {
            state,
            task: Box::new(expr(
                ExprKind::Sleep {
                    milliseconds: Box::new(constant(Constant::Int(0), Type::Int)),
                },
                Type::Task(Box::new(Type::Unit)),
            )),
        },
        Type::Unit,
    )
}

fn raw_handle_type(id: u32, name: &str) -> TypeDef {
    TypeDef {
        id: TypeId(id),
        name: name.to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "raw".to_owned(),
                ty: Type::Int,
                span: span(),
            }],
            invariant: None,
        },
    }
}

fn resource_type() -> Type {
    Type::Nominal(TypeId(0), Vec::new())
}

fn resource_value(raw: i64) -> Expr {
    expr(
        ExprKind::Record {
            ty: TypeId(0),
            type_arguments: Vec::new(),
            fields: vec![constant(Constant::Int(raw), Type::Int)],
            construction: ConstructionMode::Plain,
        },
        resource_type(),
    )
}

fn scoped_resource(local: u32, raw: i64) -> Statement {
    Statement {
        kind: StatementKind::Scoped {
            local: LocalId(local),
            value: resource_value(raw),
            disposal: ScopedDisposal::StaticConcept {
                requirement: RequirementId(0),
                witness: WitnessRef::Concrete(WitnessId(0)),
                dispatch_type: resource_type(),
            },
        },
        span: span(),
    }
}

#[allow(clippy::too_many_lines)]
fn resource_program(mut main: Function, mut extra: Vec<Function>, no_suspend: bool) -> Program {
    main.id = FunctionId(1);
    "main".clone_into(&mut main.name);
    let mut dispose = function(
        0,
        vec![local(0, resource_type(), true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    "dispose".clone_into(&mut dispose.name);
    dispose.receiver = Some(Receiver::Mutable);
    for (index, function) in extra.iter_mut().enumerate() {
        function.id = FunctionId(u32::try_from(index + 2).expect("test function id"));
    }
    let mut functions = vec![dispose, main];
    functions.extend(extra);
    let mut witnesses = vec![
        Witness {
            id: WitnessId(0),
            concept: ConceptId(0),
            concrete: resource_type(),
            methods: BTreeMap::from([(RequirementId(0), FunctionId(0))]),
            associated: BTreeMap::new(),
            type_parameters: 0,
            prerequisites: Vec::new(),
        },
        Witness {
            id: WitnessId(1),
            concept: ConceptId(1),
            concrete: resource_type(),
            methods: BTreeMap::new(),
            associated: BTreeMap::new(),
            type_parameters: 0,
            prerequisites: Vec::new(),
        },
    ];
    if no_suspend {
        witnesses.push(Witness {
            id: WitnessId(2),
            concept: ConceptId(2),
            concrete: resource_type(),
            methods: BTreeMap::new(),
            associated: BTreeMap::new(),
            type_parameters: 0,
            prerequisites: Vec::new(),
        });
    }
    Program {
        types: vec![TypeDef {
            id: TypeId(0),
            name: "Resource".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "raw".to_owned(),
                    ty: Type::Int,
                    span: span(),
                }],
                invariant: None,
            },
        }],
        concepts: vec![
            ConceptDef {
                id: ConceptId(0),
                module: "std.resource".to_owned(),
                name: "Dispose".to_owned(),
                span: span(),
                identity: Some(ConceptIdentity::Dispose),
                dynamic: false,
                associated_types: Vec::new(),
                requirements: vec![RequirementId(0)],
            },
            ConceptDef {
                id: ConceptId(1),
                module: "std.resource".to_owned(),
                name: "MustScope".to_owned(),
                span: span(),
                identity: Some(ConceptIdentity::MustScope),
                dynamic: false,
                associated_types: Vec::new(),
                requirements: Vec::new(),
            },
            ConceptDef {
                id: ConceptId(2),
                module: "std.resource".to_owned(),
                name: "NoSuspend".to_owned(),
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
            name: "dispose".to_owned(),
            span: span(),
            receiver: Some(Receiver::Mutable),
            method_type_parameters: 0,
            params: vec![RequirementType::SelfType],
            return_ty: RequirementType::Unit,
            witness_params: Vec::new(),
        }],
        functions,
        witnesses,
        exports: BTreeMap::from([("main".to_owned(), FunctionId(1))]),
        prelude: PreludeIds {
            dispose_concept: Some(ConceptId(0)),
            dispose_requirement: Some(RequirementId(0)),
            must_scope_concept: Some(ConceptId(1)),
            no_suspend_concept: Some(ConceptId(2)),
            ..PreludeIds::default()
        },
        ..Program::default()
    }
}

fn artifact_program_with_resource_identities(mut program: Program) -> CheckedProgram {
    assert!(program.prelude.dispose_concept.is_none());
    assert!(program.prelude.dispose_requirement.is_none());
    assert!(program.prelude.must_scope_concept.is_none());
    assert!(program.prelude.no_suspend_concept.is_none());

    let dispose = ConceptId(u32::try_from(program.concepts.len()).expect("test concept id"));
    let must_scope = ConceptId(dispose.0 + 1);
    let no_suspend = ConceptId(dispose.0 + 2);
    let dispose_requirement =
        RequirementId(u32::try_from(program.requirements.len()).expect("test requirement id"));
    program.concepts.extend([
        ConceptDef {
            id: dispose,
            module: "std.resource".to_owned(),
            name: "Dispose".to_owned(),
            span: span(),
            identity: Some(ConceptIdentity::Dispose),
            dynamic: false,
            associated_types: Vec::new(),
            requirements: vec![dispose_requirement],
        },
        ConceptDef {
            id: must_scope,
            module: "std.resource".to_owned(),
            name: "MustScope".to_owned(),
            span: span(),
            identity: Some(ConceptIdentity::MustScope),
            dynamic: false,
            associated_types: Vec::new(),
            requirements: Vec::new(),
        },
        ConceptDef {
            id: no_suspend,
            module: "std.resource".to_owned(),
            name: "NoSuspend".to_owned(),
            span: span(),
            identity: Some(ConceptIdentity::NoSuspend),
            dynamic: false,
            associated_types: Vec::new(),
            requirements: Vec::new(),
        },
    ]);
    program.requirements.push(RequirementDef {
        id: dispose_requirement,
        concept: dispose,
        name: "dispose".to_owned(),
        span: span(),
        receiver: Some(Receiver::Mutable),
        method_type_parameters: 0,
        params: vec![RequirementType::SelfType],
        return_ty: RequirementType::Unit,
        witness_params: Vec::new(),
    });
    program.prelude.dispose_concept = Some(dispose);
    program.prelude.dispose_requirement = Some(dispose_requirement);
    program.prelude.must_scope_concept = Some(must_scope);
    program.prelude.no_suspend_concept = Some(no_suspend);
    program
        .into_checked()
        .expect("artifact fixture has canonical resource identities")
}

fn generic_carrier_type(id: u32) -> TypeDef {
    TypeDef {
        id: TypeId(id),
        name: "Carrier".to_owned(),
        span: span(),
        type_parameters: 1,
        kind: TypeDefKind::Record {
            fields: vec![
                FieldDef {
                    name: "items".to_owned(),
                    ty: Type::List(Box::new(Type::Parameter(0))),
                    span: span(),
                },
                FieldDef {
                    name: "outcome".to_owned(),
                    ty: Type::TaskOutcome(Box::new(Type::Parameter(0))),
                    span: span(),
                },
            ],
            invariant: None,
        },
    }
}

fn empty_dyn_concept() -> ConceptDef {
    ConceptDef {
        id: ConceptId(0),
        module: "test".to_owned(),
        name: "Viewable".to_owned(),
        span: span(),
        identity: None,
        dynamic: true,
        associated_types: Vec::new(),
        requirements: Vec::new(),
    }
}

fn view_type(mutable: bool) -> Type {
    Type::View {
        mutable,
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
    }
}

fn empty_witness(id: u32, concrete: Type) -> Witness {
    Witness {
        id: WitnessId(id),
        concept: ConceptId(0),
        concrete,
        methods: BTreeMap::new(),
        associated: BTreeMap::new(),
        type_parameters: 0,
        prerequisites: Vec::new(),
    }
}

fn borrowed_view(place: Place, witness: u32, token: u32, mutable: bool) -> Expr {
    expr(
        ExprKind::MakeView {
            value: Box::new(copy_place(place.clone(), Type::Int)),
            writeback: Some(place),
            witness: WitnessRef::Concrete(WitnessId(witness)),
            mutable,
            token,
        },
        view_type(mutable),
    )
}

#[test]
fn valid_program_crosses_checked_boundary() {
    let program = simple_program();
    validate_program(&program).expect("valid MIR");
    let checked = CheckedProgram::new(program).expect("checked MIR");
    assert_eq!(checked.functions.len(), 1);
}

#[test]
fn checked_boundary_scopes_break_and_continue_to_the_nearest_loop() {
    let loop_body = Block {
        statements: vec![Statement {
            kind: StatementKind::Break,
            span: span(),
        }],
        tail: None,
        span: span(),
    };
    let valid = Program {
        functions: vec![function(
            0,
            Vec::new(),
            Vec::new(),
            Type::Unit,
            Block {
                statements: vec![Statement {
                    kind: StatementKind::While {
                        condition: Box::new(constant(Constant::Bool(true), Type::Bool)),
                        body: Box::new(loop_body),
                    },
                    span: span(),
                }],
                tail: None,
                span: span(),
            },
        )],
        ..Program::default()
    };
    validate_program(&valid).expect("break inside while is valid checked MIR");

    for kind in [StatementKind::Break, StatementKind::Continue] {
        let invalid = Program {
            functions: vec![function(
                0,
                Vec::new(),
                Vec::new(),
                Type::Unit,
                Block {
                    statements: vec![Statement { kind, span: span() }],
                    tail: None,
                    span: span(),
                },
            )],
            ..Program::default()
        };
        assert!(
            validation_errors(&invalid).contains(MirValidationCode::ExpressionShape),
            "loop control outside a loop must fail closed"
        );
    }
}

#[test]
fn cleanup_loop_control_keeps_loop_depth_through_nested_expressions() {
    let control_block = |kind| Block {
        statements: vec![Statement { kind, span: span() }],
        tail: None,
        span: span(),
    };
    let nested_block = expr(
        ExprKind::Block(control_block(StatementKind::Break)),
        Type::Never,
    );
    let nested_if = expr(
        ExprKind::If {
            condition: Box::new(constant(Constant::Bool(true), Type::Bool)),
            then_branch: control_block(StatementKind::Continue),
            else_branch: control_block(StatementKind::Continue),
        },
        Type::Never,
    );
    let nested_match = expr(
        ExprKind::Match {
            scrutinee: Box::new(constant(Constant::Bool(true), Type::Bool)),
            arms: vec![
                MatchArm {
                    pattern: Pattern::Constant(Constant::Bool(true)),
                    bindings: Vec::new(),
                    value: expr(
                        ExprKind::Block(control_block(StatementKind::Break)),
                        Type::Never,
                    ),
                },
                MatchArm {
                    pattern: Pattern::Constant(Constant::Bool(false)),
                    bindings: Vec::new(),
                    value: expr(
                        ExprKind::Block(control_block(StatementKind::Break)),
                        Type::Never,
                    ),
                },
            ],
        },
        Type::Never,
    );
    let loop_statement = |expression| Statement {
        kind: StatementKind::While {
            condition: Box::new(constant(Constant::Bool(true), Type::Bool)),
            body: Box::new(Block {
                statements: vec![Statement {
                    kind: StatementKind::Evaluate(expression),
                    span: span(),
                }],
                tail: None,
                span: span(),
            }),
        },
        span: span(),
    };
    let cleanup = Block {
        statements: vec![
            loop_statement(nested_block),
            loop_statement(nested_if),
            loop_statement(nested_match),
        ],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let program = Program {
        functions: vec![function(
            0,
            Vec::new(),
            Vec::new(),
            Type::Unit,
            Block {
                statements: vec![Statement {
                    kind: StatementKind::Defer(cleanup),
                    span: span(),
                }],
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        ..Program::default()
    };
    validate_program(&program)
        .expect("cleanup-local loop control remains valid through Block, If, and Match");
}

fn while_condition_that_moves(local: u32) -> Expr {
    expr(
        ExprKind::Block(Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(expr(
                    ExprKind::Move(Place::local(LocalId(local))),
                    Type::Int,
                )),
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Bool(true), Type::Bool))),
            span: span(),
        }),
        Type::Bool,
    )
}

#[test]
fn while_backedge_must_restore_values_consumed_by_its_condition() {
    let program = Program {
        functions: vec![function(
            0,
            Vec::new(),
            vec![local(0, Type::Int, true)],
            Type::Unit,
            Block {
                statements: vec![
                    Statement {
                        kind: StatementKind::Let {
                            local: LocalId(0),
                            value: constant(Constant::Int(1), Type::Int),
                        },
                        span: span(),
                    },
                    Statement {
                        kind: StatementKind::While {
                            condition: Box::new(while_condition_that_moves(0)),
                            body: Box::new(Block {
                                statements: Vec::new(),
                                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                                span: span(),
                            }),
                        },
                        span: span(),
                    },
                ],
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        ..Program::default()
    };
    let errors = validation_errors(&program);
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::LocalState
            && error.message.contains("continuing While body")
    }));
}

#[test]
fn while_body_may_restore_a_value_consumed_by_its_condition() {
    let program = Program {
        functions: vec![function(
            0,
            Vec::new(),
            vec![local(0, Type::Int, true)],
            Type::Unit,
            Block {
                statements: vec![
                    Statement {
                        kind: StatementKind::Let {
                            local: LocalId(0),
                            value: constant(Constant::Int(1), Type::Int),
                        },
                        span: span(),
                    },
                    Statement {
                        kind: StatementKind::While {
                            condition: Box::new(while_condition_that_moves(0)),
                            body: Box::new(Block {
                                statements: vec![Statement {
                                    kind: StatementKind::Assign {
                                        place: Place::local(LocalId(0)),
                                        value: constant(Constant::Int(2), Type::Int),
                                    },
                                    span: span(),
                                }],
                                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                                span: span(),
                            }),
                        },
                        span: span(),
                    },
                ],
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        ..Program::default()
    };
    validate_program(&program).expect("the body restores the condition-consumed value");
}

#[test]
fn expression_ids_are_function_local_dense_canonical_preorder() {
    let first = function(
        0,
        vec![local(0, Type::Int, false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Assert {
                        condition: expr(
                            ExprKind::Binary(
                                loom_mir::BinaryOp::Equal,
                                Box::new(copy(0, Type::Int)),
                                Box::new(constant(Constant::Int(7), Type::Int)),
                            ),
                            Type::Bool,
                        ),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Block(Block {
                            statements: vec![Statement {
                                kind: StatementKind::Evaluate(constant(Constant::Unit, Type::Unit)),
                                span: span(),
                            }],
                            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                            span: span(),
                        }),
                        Type::Unit,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(expr(
                ExprKind::Binary(
                    loom_mir::BinaryOp::Add,
                    Box::new(copy(0, Type::Int)),
                    Box::new(constant(Constant::Int(1), Type::Int)),
                ),
                Type::Int,
            ))),
            span: span(),
        },
    );
    let second = function(
        1,
        Vec::new(),
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let program = Program {
        functions: vec![first, second],
        ..Program::default()
    };

    assert_eq!(
        program.functions[0]
            .exprs_preorder()
            .map(|expression| expression.id.0)
            .collect::<Vec<_>>(),
        (0..9).collect::<Vec<_>>()
    );
    assert_eq!(
        program.functions[1]
            .exprs_preorder()
            .map(|expression| expression.id.0)
            .collect::<Vec<_>>(),
        vec![0]
    );
    validate_program(&program).expect("canonical expression identities validate");
}

#[test]
#[allow(clippy::too_many_lines)]
fn assigner_and_canonical_walker_agree_for_every_expression_shape() {
    let unit_block = || Block {
        statements: Vec::new(),
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let shapes = vec![
        constant(Constant::Unit, Type::Unit),
        expr(
            ExprKind::Tuple(vec![
                constant(Constant::Int(1), Type::Int),
                constant(Constant::Bool(true), Type::Bool),
            ]),
            Type::Tuple(vec![Type::Int, Type::Bool]),
        ),
        expr(
            ExprKind::List(vec![
                constant(Constant::Int(1), Type::Int),
                constant(Constant::Int(2), Type::Int),
            ]),
            Type::List(Box::new(Type::Int)),
        ),
        copy(0, Type::Int),
        expr(ExprKind::Move(Place::local(LocalId(0))), Type::Int),
        expr(
            ExprKind::Unary(
                loom_mir::UnaryOp::Negate,
                Box::new(constant(Constant::Int(1), Type::Int)),
            ),
            Type::Int,
        ),
        expr(
            ExprKind::Binary(
                loom_mir::BinaryOp::Add,
                Box::new(constant(Constant::Int(1), Type::Int)),
                Box::new(constant(Constant::Int(2), Type::Int)),
            ),
            Type::Int,
        ),
        expr(
            ExprKind::Block(Block {
                statements: vec![Statement {
                    kind: StatementKind::Evaluate(constant(Constant::Unit, Type::Unit)),
                    span: span(),
                }],
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            }),
            Type::Unit,
        ),
        expr(
            ExprKind::If {
                condition: Box::new(constant(Constant::Bool(true), Type::Bool)),
                then_branch: unit_block(),
                else_branch: unit_block(),
            },
            Type::Unit,
        ),
        expr(
            ExprKind::Match {
                scrutinee: Box::new(constant(Constant::Bool(true), Type::Bool)),
                arms: vec![
                    MatchArm {
                        pattern: Pattern::Constant(Constant::Bool(true)),
                        bindings: Vec::new(),
                        value: constant(Constant::Unit, Type::Unit),
                    },
                    MatchArm {
                        pattern: Pattern::Constant(Constant::Bool(false)),
                        bindings: Vec::new(),
                        value: constant(Constant::Unit, Type::Unit),
                    },
                ],
            },
            Type::Unit,
        ),
        expr(
            ExprKind::Record {
                ty: TypeId(0),
                type_arguments: Vec::new(),
                fields: vec![
                    constant(Constant::Int(1), Type::Int),
                    constant(Constant::Int(2), Type::Int),
                ],
                construction: ConstructionMode::Plain,
            },
            Type::Nominal(TypeId(0), Vec::new()),
        ),
        expr(
            ExprKind::Variant {
                ty: TypeId(1),
                type_arguments: Vec::new(),
                variant: VariantId(0),
                payload: vec![
                    constant(Constant::Int(1), Type::Int),
                    constant(Constant::Int(2), Type::Int),
                ],
            },
            Type::Nominal(TypeId(1), Vec::new()),
        ),
        expr(
            ExprKind::Refine {
                ty: TypeId(2),
                value: Box::new(constant(Constant::Int(1), Type::Int)),
                construction: ConstructionMode::Proven,
            },
            Type::Nominal(TypeId(2), Vec::new()),
        ),
        expr(
            ExprKind::Unrefine(Box::new(copy(0, Type::Nominal(TypeId(2), Vec::new())))),
            Type::Int,
        ),
        expr(
            ExprKind::Call {
                target: CallTarget::Builtin(loom_mir::Builtin::FloatIsFinite),
                type_arguments: Vec::new(),
                arguments: vec![
                    CallArgument::Value(constant(Constant::Float(1.0), Type::Float)),
                    CallArgument::InOut(Place::local(LocalId(0))),
                    CallArgument::Value(constant(Constant::Float(2.0), Type::Float)),
                ],
                witnesses: Vec::new(),
            },
            Type::Bool,
        ),
        expr(
            ExprKind::MakeView {
                value: Box::new(copy(0, Type::Int)),
                writeback: None,
                witness: WitnessRef::Concrete(WitnessId(0)),
                mutable: false,
                token: 0,
            },
            Type::View {
                mutable: false,
                concept: ConceptId(0),
                bindings: BTreeMap::new(),
            },
        ),
        expr(
            ExprKind::ReborrowView {
                owner: Place::local(LocalId(0)),
                mutable: false,
                token: 1,
            },
            Type::View {
                mutable: false,
                concept: ConceptId(0),
                bindings: BTreeMap::new(),
            },
        ),
        expr(
            ExprKind::Await {
                state: 1,
                task: Box::new(copy(0, Type::Task(Box::new(Type::Unit)))),
            },
            Type::Unit,
        ),
        expr(
            ExprKind::Sleep {
                milliseconds: Box::new(constant(Constant::Int(1), Type::Int)),
            },
            Type::Task(Box::new(Type::Unit)),
        ),
        expr(
            ExprKind::TaskJoin {
                mode: loom_mir::TaskJoinMode::All,
                arguments: vec![
                    copy(0, Type::Task(Box::new(Type::Unit))),
                    copy(0, Type::Task(Box::new(Type::Unit))),
                ],
            },
            Type::Task(Box::new(Type::Tuple(vec![Type::Unit, Type::Unit]))),
        ),
    ];
    let function = function(
        0,
        vec![local(0, Type::Int, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: shapes
                .into_iter()
                .map(|expression| Statement {
                    kind: StatementKind::Evaluate(expression),
                    span: span(),
                })
                .collect(),
            tail: None,
            span: span(),
        },
    );

    assert_eq!(
        function
            .exprs_preorder()
            .map(|expression| expression.id.0)
            .collect::<Vec<_>>(),
        (0..48).collect::<Vec<_>>()
    );
}

#[test]
fn checked_boundary_rejects_duplicate_gapped_reordered_and_unassigned_expr_ids() {
    let mut duplicate = simple_program();
    let StatementKind::Let { value, .. } = &mut duplicate.functions[0].body.statements[0].kind
    else {
        panic!("simple program starts with Let");
    };
    value.id = ExprId(1);
    assert!(validation_errors(&duplicate).contains(MirValidationCode::ExpressionIdentity));

    let mut gapped = simple_program();
    gapped.functions[0]
        .body
        .tail
        .as_mut()
        .expect("simple tail")
        .id = ExprId(2);
    assert!(validation_errors(&gapped).contains(MirValidationCode::ExpressionIdentity));

    let mut reordered = simple_program();
    let StatementKind::Let { value, .. } = &mut reordered.functions[0].body.statements[0].kind
    else {
        panic!("simple program starts with Let");
    };
    value.id = ExprId(1);
    reordered.functions[0]
        .body
        .tail
        .as_mut()
        .expect("simple tail")
        .id = ExprId(0);
    assert!(validation_errors(&reordered).contains(MirValidationCode::ExpressionIdentity));

    let mut unassigned = simple_program();
    let StatementKind::Let { value, .. } = &mut unassigned.functions[0].body.statements[0].kind
    else {
        panic!("simple program starts with Let");
    };
    value.id = ExprId::UNASSIGNED;
    assert!(validation_errors(&unassigned).contains(MirValidationCode::ExpressionIdentity));
}

#[test]
fn checked_boundary_rejects_discarded_tasks_resources_and_unknown_generics() {
    let file = Type::Nominal(TypeId(0), Vec::new());
    let socket = Type::Nominal(TypeId(1), Vec::new());
    let carrier = |argument| Type::Nominal(TypeId(2), vec![argument]);
    let phantom = |argument| Type::Nominal(TypeId(3), vec![argument]);
    let parameter = Type::Parameter(0);
    let parameter_types = vec![
        Type::Task(Box::new(Type::Int)),
        carrier(Type::Task(Box::new(Type::Int))),
        carrier(file),
        socket,
        parameter.clone(),
        carrier(parameter.clone()),
        phantom(parameter),
        Type::Unit,
    ];
    let mut checked = function(
        0,
        (0_u32..)
            .zip(&parameter_types)
            .map(|(index, ty)| local(index, ty.clone(), false))
            .collect(),
        Vec::new(),
        Type::Unit,
        Block {
            statements: (0_u32..)
                .zip(parameter_types)
                .map(|(index, ty)| Statement {
                    kind: StatementKind::Evaluate(copy(index, ty)),
                    span: span(),
                })
                .collect(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    checked.type_parameters = 1;

    let errors = validation_errors(&Program {
        types: vec![
            raw_handle_type(0, "File"),
            raw_handle_type(1, "Socket"),
            generic_carrier_type(2),
            TypeDef {
                id: TypeId(3),
                name: "Phantom".to_owned(),
                span: span(),
                type_parameters: 1,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "marker".to_owned(),
                        ty: Type::Int,
                        span: span(),
                    }],
                    invariant: None,
                },
            },
        ],
        functions: vec![checked],
        prelude: PreludeIds {
            file: Some(TypeId(0)),
            socket: Some(TypeId(1)),
            ..PreludeIds::default()
        },
        ..Program::default()
    });
    let obligation_errors = errors
        .iter()
        .filter(|error| error.code == MirValidationCode::ObligationShape)
        .collect::<Vec<_>>();
    assert_eq!(obligation_errors.len(), 6, "{errors:?}");
    assert!(
        obligation_errors
            .iter()
            .any(|error| error.message.contains("unconsumed Task"))
    );
    assert!(
        obligation_errors
            .iter()
            .any(|error| error.message.contains("File or Socket"))
    );
    assert!(
        obligation_errors
            .iter()
            .any(|error| error.message.contains("unresolved generic"))
    );
}

#[test]
fn obligation_hardening_allows_unit_divergence_and_phantom_parameters() {
    let parameter = Type::Parameter(0);
    let phantom = Type::Nominal(TypeId(0), vec![parameter]);
    let mut checked = function(
        0,
        vec![local(0, phantom.clone(), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(copy(0, phantom)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(constant(Constant::Unit, Type::Unit)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr {
                        id: ExprId::UNASSIGNED,
                        kind: ExprKind::Block(Block {
                            statements: vec![Statement {
                                kind: StatementKind::Return(None),
                                span: span(),
                            }],
                            tail: None,
                            span: span(),
                        }),
                        // A diverging expression produces no Task to discard even when
                        // flow compatibility gives the expression a Task result type.
                        ty: Type::Task(Box::new(Type::Int)),
                        span: span(),
                    }),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    checked.type_parameters = 1;
    let program = Program {
        types: vec![TypeDef {
            id: TypeId(0),
            name: "Phantom".to_owned(),
            span: span(),
            type_parameters: 1,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "marker".to_owned(),
                    ty: Type::Int,
                    span: span(),
                }],
                invariant: None,
            },
        }],
        functions: vec![checked],
        ..Program::default()
    };
    validate_program(&program).expect("safe Evaluate forms must remain valid");
}

#[test]
#[allow(clippy::too_many_lines)]
fn checked_boundary_rejects_dynamic_erasure_of_provable_obligations() {
    let concept = ConceptDef {
        id: ConceptId(0),
        module: "test".to_owned(),
        name: "Display".to_owned(),
        span: span(),
        identity: None,
        dynamic: true,
        associated_types: Vec::new(),
        requirements: Vec::new(),
    };
    let view = Type::View {
        mutable: false,
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
    };
    let task = Type::Task(Box::new(Type::Int));
    let carried_file = Type::Nominal(TypeId(1), vec![Type::Nominal(TypeId(0), Vec::new())]);
    let make_view = |local_id, ty: Type, witness, token| Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::MakeView {
            value: Box::new(copy(local_id, ty)),
            writeback: None,
            witness,
            mutable: false,
            token,
        },
        ty: view.clone(),
        span: span(),
    };
    let concrete = function(
        0,
        vec![
            local(0, task.clone(), false),
            local(1, carried_file.clone(), false),
        ],
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(make_view(
                        0,
                        task.clone(),
                        WitnessRef::Concrete(WitnessId(0)),
                        1,
                    )),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(make_view(
                        1,
                        carried_file.clone(),
                        WitnessRef::Concrete(WitnessId(1)),
                        2,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let parameter = Type::Parameter(0);
    let mut generic = function(
        1,
        vec![local(0, parameter.clone(), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(make_view(
                    0,
                    parameter.clone(),
                    WitnessRef::Parameter(0),
                    3,
                )),
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    generic.type_parameters = 1;
    generic.witness_params.push(WitnessParam {
        target: parameter,
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
        span: span(),
    });

    let errors = validation_errors(&Program {
        types: vec![raw_handle_type(0, "File"), generic_carrier_type(1)],
        concepts: vec![concept],
        functions: vec![concrete, generic],
        witnesses: vec![
            Witness {
                id: WitnessId(0),
                concept: ConceptId(0),
                concrete: task,
                methods: BTreeMap::new(),
                associated: BTreeMap::new(),
                type_parameters: 0,
                prerequisites: Vec::new(),
            },
            Witness {
                id: WitnessId(1),
                concept: ConceptId(0),
                concrete: carried_file,
                methods: BTreeMap::new(),
                associated: BTreeMap::new(),
                type_parameters: 0,
                prerequisites: Vec::new(),
            },
        ],
        prelude: PreludeIds {
            file: Some(TypeId(0)),
            ..PreludeIds::default()
        },
        ..Program::default()
    });
    let obligation_errors = errors
        .iter()
        .filter(|error| error.code == MirValidationCode::ObligationShape)
        .collect::<Vec<_>>();
    assert_eq!(obligation_errors.len(), 3, "{errors:?}");
    assert!(obligation_errors.iter().all(|error| {
        error
            .message
            .contains("erase into a dynamic concept interface")
    }));
}

#[test]
fn generic_equality_cannot_cross_the_checked_mir_boundary() {
    let parameter = Type::Parameter(0);
    let mut generic = function(
        0,
        vec![
            local(0, parameter.clone(), false),
            local(1, parameter.clone(), false),
        ],
        Vec::new(),
        Type::Bool,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Binary(
                    loom_mir::BinaryOp::Equal,
                    Box::new(copy(0, parameter.clone())),
                    Box::new(copy(1, parameter)),
                ),
                ty: Type::Bool,
                span: span(),
            })),
            span: span(),
        },
    );
    generic.type_parameters = 1;
    let program = Program {
        functions: vec![generic],
        ..Program::default()
    };
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::ExpressionShape));
}

#[test]
fn direct_indices_are_validated_before_interpretation() {
    let mut program = simple_program();
    program.functions[0].id = FunctionId(9);
    program.functions[0].return_ty = Type::Nominal(TypeId(22), Vec::new());
    program.tests.push(FunctionId(44));
    program.exports.insert("bad".to_owned(), FunctionId(55));
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::IndexMismatch));
    assert!(errors.contains(MirValidationCode::InvalidTypeReference));
    assert!(errors.contains(MirValidationCode::InvalidFunctionReference));
}

#[test]
fn calls_validate_value_and_witness_arity() {
    let mut target_function = function(
        0,
        vec![local(0, Type::Int, false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(copy(0, Type::Int))),
            span: span(),
        },
    );
    target_function.witness_params.push(WitnessParam {
        target: Type::Int,
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
        span: span(),
    });
    let calling_function = function(
        1,
        Vec::new(),
        Vec::new(),
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(0)),
                    type_arguments: Vec::new(),
                    arguments: Vec::new(),
                    witnesses: Vec::new(),
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
    );
    let program = Program {
        functions: vec![target_function, calling_function],
        ..Program::default()
    };
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::CallArity));
    assert!(errors.contains(MirValidationCode::WitnessArity));
}

#[test]
fn locals_and_places_are_checked_without_indexing_panics() {
    let mut program = simple_program();
    program.functions[0].body.statements.push(Statement {
        kind: StatementKind::Assign {
            place: Place::local(LocalId(0)),
            value: constant(Constant::Int(1), Type::Int),
        },
        span: span(),
    });
    program.functions[0].body.tail = Some(Box::new(Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Move(Place {
            local: LocalId(99),
            projection: vec![3],
        }),
        ty: Type::Int,
        span: span(),
    }));
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::ImmutablePlace));
    assert!(errors.contains(MirValidationCode::InvalidLocalReference));
}

#[test]
fn projected_move_returns_the_leaf_and_consumes_the_complete_root() {
    let pair = Type::Nominal(TypeId(0), Vec::new());
    let pair_definition = TypeDef {
        id: TypeId(0),
        name: "Pair".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![
                FieldDef {
                    name: "left".to_owned(),
                    ty: Type::Int,
                    span: span(),
                },
                FieldDef {
                    name: "right".to_owned(),
                    ty: Type::Int,
                    span: span(),
                },
            ],
            invariant: None,
        },
    };
    let projected = Place {
        local: LocalId(0),
        projection: vec![1],
    };
    let valid = function(
        0,
        vec![local(0, pair.clone(), false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(expr(ExprKind::Move(projected.clone()), Type::Int))),
            span: span(),
        },
    );
    validate_program(&Program {
        types: vec![pair_definition.clone()],
        functions: vec![valid],
        ..Program::default()
    })
    .expect("a projected move consumes its direct aggregate root");

    let invalid_reuse = function(
        0,
        vec![local(0, pair.clone(), false)],
        vec![local(1, Type::Int, false)],
        pair.clone(),
        Block {
            statements: vec![Statement {
                kind: StatementKind::Let {
                    local: LocalId(1),
                    value: expr(ExprKind::Move(projected), Type::Int),
                },
                span: span(),
            }],
            tail: Some(Box::new(copy(0, pair))),
            span: span(),
        },
    );
    let errors = validation_errors(&Program {
        types: vec![pair_definition],
        functions: vec![invalid_reuse],
        ..Program::default()
    });
    assert!(errors.contains(MirValidationCode::LocalState));
}

fn shape_types() -> Vec<TypeDef> {
    vec![
        TypeDef {
            id: TypeId(0),
            name: "Pair".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "value".to_owned(),
                    ty: Type::Int,
                    span: span(),
                }],
                invariant: None,
            },
        },
        TypeDef {
            id: TypeId(1),
            name: "Choice".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Enum {
                variants: vec![VariantDef {
                    id: VariantId(0),
                    name: "Text".to_owned(),
                    payload: vec![Type::Text],
                    span: span(),
                }],
            },
        },
    ]
}

#[test]
fn record_and_variant_shapes_are_validated() {
    let record = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Record {
            ty: TypeId(0),
            type_arguments: Vec::new(),
            fields: Vec::new(),
            construction: ConstructionMode::Plain,
        },
        ty: Type::Nominal(TypeId(0), Vec::new()),
        span: span(),
    };
    let variant = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Variant {
            ty: TypeId(1),
            type_arguments: Vec::new(),
            variant: VariantId(7),
            payload: vec![constant(Constant::Int(1), Type::Int)],
        },
        ty: Type::Nominal(TypeId(1), Vec::new()),
        span: span(),
    };
    let program = Program {
        types: shape_types(),
        functions: vec![function(
            0,
            Vec::new(),
            Vec::new(),
            Type::Unit,
            Block {
                statements: vec![
                    Statement {
                        kind: StatementKind::Evaluate(record),
                        span: span(),
                    },
                    Statement {
                        kind: StatementKind::Evaluate(variant),
                        span: span(),
                    },
                ],
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        ..Program::default()
    };
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::RecordShape));
    assert!(errors.contains(MirValidationCode::InvalidVariantReference));
}

#[test]
#[allow(clippy::too_many_lines)]
fn checked_construction_modes_are_a_validated_trust_boundary() {
    let always = || Contract {
        code: "always".to_owned(),
        span: span(),
        expression: ContractExpr {
            kind: ContractExprKind::Constant(Constant::Bool(true)),
            span: span(),
        },
    };
    let result = TypeId(0);
    let constraint_error = TypeId(1);
    let money = TypeId(2);
    let guarded = TypeId(3);
    let plain = TypeId(4);
    let types = vec![
        TypeDef {
            id: result,
            name: "Result".to_owned(),
            span: span(),
            type_parameters: 2,
            kind: TypeDefKind::Enum {
                variants: vec![
                    VariantDef {
                        id: VariantId(0),
                        name: "Ok".to_owned(),
                        payload: vec![Type::Parameter(0)],
                        span: span(),
                    },
                    VariantDef {
                        id: VariantId(1),
                        name: "Err".to_owned(),
                        payload: vec![Type::Parameter(1)],
                        span: span(),
                    },
                ],
            },
        },
        TypeDef {
            id: constraint_error,
            name: "ConstraintError".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: constraint_error_fields(),
                invariant: None,
            },
        },
        TypeDef {
            id: money,
            name: "Money".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Refined {
                base: Type::Float,
                predicate: always(),
            },
        },
        TypeDef {
            id: guarded,
            name: "Guarded".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "value".to_owned(),
                    ty: Type::Int,
                    span: span(),
                }],
                invariant: Some(always()),
            },
        },
        TypeDef {
            id: plain,
            name: "Plain".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "value".to_owned(),
                    ty: Type::Int,
                    span: span(),
                }],
                invariant: None,
            },
        },
    ];
    let result_of = |success| {
        Type::Nominal(
            result,
            vec![
                Type::Nominal(success, Vec::new()),
                Type::Nominal(constraint_error, Vec::new()),
            ],
        )
    };
    let expressions = vec![
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Refine {
                ty: money,
                value: Box::new(constant(Constant::Float(1.0), Type::Float)),
                construction: ConstructionMode::Proven,
            },
            ty: Type::Nominal(money, Vec::new()),
            span: span(),
        },
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Refine {
                ty: money,
                value: Box::new(constant(Constant::Float(1.0), Type::Float)),
                construction: ConstructionMode::Runtime,
            },
            ty: result_of(money),
            span: span(),
        },
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Record {
                ty: guarded,
                type_arguments: Vec::new(),
                fields: vec![constant(Constant::Int(1), Type::Int)],
                construction: ConstructionMode::Proven,
            },
            ty: Type::Nominal(guarded, Vec::new()),
            span: span(),
        },
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Record {
                ty: guarded,
                type_arguments: Vec::new(),
                fields: vec![constant(Constant::Int(1), Type::Int)],
                construction: ConstructionMode::Runtime,
            },
            ty: result_of(guarded),
            span: span(),
        },
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Record {
                ty: plain,
                type_arguments: Vec::new(),
                fields: vec![constant(Constant::Int(1), Type::Int)],
                construction: ConstructionMode::Plain,
            },
            ty: Type::Nominal(plain, Vec::new()),
            span: span(),
        },
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Refine {
                ty: money,
                value: Box::new(constant(Constant::Float(1.0), Type::Float)),
                construction: ConstructionMode::Recheck,
            },
            ty: Type::Nominal(money, Vec::new()),
            span: span(),
        },
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Record {
                ty: guarded,
                type_arguments: Vec::new(),
                fields: vec![constant(Constant::Int(1), Type::Int)],
                construction: ConstructionMode::Recheck,
            },
            ty: Type::Nominal(guarded, Vec::new()),
            span: span(),
        },
    ];
    let mut program = Program {
        types,
        functions: vec![function(
            0,
            Vec::new(),
            Vec::new(),
            Type::Unit,
            Block {
                statements: expressions
                    .into_iter()
                    .map(|expression| Statement {
                        kind: StatementKind::Evaluate(expression),
                        span: span(),
                    })
                    .collect(),
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        prelude: PreludeIds {
            result: Some(result),
            constraint_error: Some(constraint_error),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    validate_program(&program).expect("valid checked construction modes");

    let mut wrong_constraint_error_shape = program.clone();
    let TypeDefKind::Record { fields, .. } =
        &mut wrong_constraint_error_shape.types[constraint_error.0 as usize].kind
    else {
        unreachable!();
    };
    fields[4].ty = Type::Int;
    assert!(
        validation_errors(&wrong_constraint_error_shape).contains(MirValidationCode::RecordShape),
        "ConstraintError must retain its exact compiler-private six-field shape"
    );

    let mut wrong_recheck_shape = program.clone();
    let StatementKind::Evaluate(expression) =
        &mut wrong_recheck_shape.functions[0].body.statements[5].kind
    else {
        unreachable!();
    };
    expression.ty = result_of(money);
    assert!(
        validation_errors(&wrong_recheck_shape).contains(MirValidationCode::TypeMismatch),
        "proof rechecks must preserve direct nominal shape rather than the source-facing Result shape"
    );

    if let StatementKind::Evaluate(Expr {
        kind: ExprKind::Refine { construction, .. },
        ..
    }) = &mut program.functions[0].body.statements[0].kind
    {
        *construction = ConstructionMode::Plain;
    } else {
        unreachable!();
    }
    assert!(validation_errors(&program).contains(MirValidationCode::ExpressionShape));

    if let StatementKind::Evaluate(Expr {
        kind: ExprKind::Refine { construction, .. },
        ..
    }) = &mut program.functions[0].body.statements[0].kind
    {
        *construction = ConstructionMode::Proven;
    } else {
        unreachable!();
    }
    let StatementKind::Evaluate(expression) = &mut program.functions[0].body.statements[4].kind
    else {
        unreachable!();
    };
    let ExprKind::Record { construction, .. } = &mut expression.kind else {
        unreachable!();
    };
    *construction = ConstructionMode::Proven;
    assert!(validation_errors(&program).contains(MirValidationCode::RecordShape));
}

#[test]
fn witness_references_and_method_proof_arity_are_validated() {
    let mut method = function(
        0,
        Vec::new(),
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    method.witness_params.push(WitnessParam {
        target: Type::Int,
        concept: ConceptId(1),
        bindings: BTreeMap::new(),
        span: span(),
    });
    let witness = Witness {
        id: WitnessId(0),
        concept: ConceptId(0),
        concrete: Type::Int,
        methods: BTreeMap::from([(RequirementId(0), FunctionId(0))]),
        associated: BTreeMap::new(),
        type_parameters: 0,
        prerequisites: Vec::new(),
    };
    let program = Program {
        functions: vec![method],
        witnesses: vec![witness],
        ..Program::default()
    };
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::InvalidConceptReference));
}

#[test]
fn witness_parameter_references_are_bounded_by_the_current_function() {
    let program = Program {
        types: shape_types(),
        functions: vec![function(
            0,
            vec![local(0, Type::Nominal(TypeId(0), Vec::new()), false)],
            Vec::new(),
            Type::View {
                mutable: false,
                concept: ConceptId(0),
                bindings: BTreeMap::new(),
            },
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::MakeView {
                        value: Box::new(copy(0, Type::Nominal(TypeId(0), Vec::new()))),
                        writeback: None,
                        witness: WitnessRef::Parameter(0),
                        mutable: false,
                        token: 1,
                    },
                    ty: Type::View {
                        mutable: false,
                        concept: ConceptId(0),
                        bindings: BTreeMap::new(),
                    },
                    span: span(),
                })),
                span: span(),
            },
        )],
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::InvalidWitnessReference));
}

#[test]
fn expression_and_contract_type_shapes_are_checked() {
    let mut program = simple_program();
    program.functions[0].body.tail = Some(Box::new(constant(Constant::Int(1), Type::Bool)));
    program.functions[0].call_plan.requires.push(Contract {
        code: "bad_argument".to_owned(),
        span: span(),
        expression: ContractExpr {
            kind: ContractExprKind::Value(ContractValue::Argument(99)),
            span: span(),
        },
    });
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::TypeMismatch));
    assert!(errors.contains(MirValidationCode::ContractShape));
}

#[test]
fn prelude_ids_are_explicit_and_shape_checked() {
    let program = Program {
        types: shape_types(),
        prelude: PreludeIds {
            result: Some(TypeId(0)),
            option: Some(TypeId(99)),
            constraint_error: Some(TypeId(1)),
            task_fault: None,
            task_outcome: None,
            duration: None,
            file: None,
            socket: None,
            bytes: None,
            path: None,
            decode_text_error: None,
            path_error: None,
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::InvalidTypeReference));
    assert!(errors.contains(MirValidationCode::RecordShape));
}

#[test]
fn text_get_rejects_a_non_integer_index_at_the_checked_boundary() {
    let program = Program {
        functions: vec![function(
            0,
            vec![local(0, Type::Text, false), local(1, Type::Text, false)],
            Vec::new(),
            Type::Int,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Call {
                        target: CallTarget::Builtin(loom_mir::Builtin::TextGet),
                        type_arguments: Vec::new(),
                        arguments: vec![
                            CallArgument::Value(copy(0, Type::Text)),
                            CallArgument::Value(copy(1, Type::Text)),
                        ],
                        witnesses: Vec::new(),
                    },
                    ty: Type::Int,
                    span: span(),
                })),
                span: span(),
            },
        )],
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::BuiltinShape));
}

#[test]
fn text_map_insert_rejects_a_wrong_value_type_at_the_checked_boundary() {
    let map = TypeId(0);
    let map_int = Type::Nominal(map, vec![Type::Int]);
    let program = Program {
        types: vec![TypeDef {
            id: map,
            name: "TextMap".to_owned(),
            span: span(),
            type_parameters: 1,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "raw".to_owned(),
                    ty: Type::Int,
                    span: span(),
                }],
                invariant: None,
            },
        }],
        functions: vec![function(
            0,
            vec![
                local(0, map_int.clone(), false),
                local(1, Type::Text, false),
                local(2, Type::Text, false),
            ],
            Vec::new(),
            map_int.clone(),
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Call {
                        target: CallTarget::Builtin(loom_mir::Builtin::TextMapInsert),
                        type_arguments: Vec::new(),
                        arguments: vec![
                            CallArgument::Value(copy(0, map_int.clone())),
                            CallArgument::Value(copy(1, Type::Text)),
                            CallArgument::Value(copy(2, Type::Text)),
                        ],
                        witnesses: Vec::new(),
                    },
                    ty: map_int,
                    span: span(),
                })),
                span: span(),
            },
        )],
        prelude: PreludeIds {
            text_map: Some(map),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::BuiltinShape));
}

#[test]
fn text_map_entry_at_rejects_a_non_integer_index_at_the_checked_boundary() {
    let map = TypeId(0);
    let map_int = Type::Nominal(map, vec![Type::Int]);
    let program = Program {
        types: vec![TypeDef {
            id: map,
            name: "TextMap".to_owned(),
            span: span(),
            type_parameters: 1,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "raw".to_owned(),
                    ty: Type::Int,
                    span: span(),
                }],
                invariant: None,
            },
        }],
        functions: vec![function(
            0,
            vec![
                local(0, map_int.clone(), false),
                local(1, Type::Text, false),
            ],
            Vec::new(),
            Type::Int,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Call {
                        target: CallTarget::Builtin(loom_mir::Builtin::TextMapEntryAt),
                        type_arguments: Vec::new(),
                        arguments: vec![
                            CallArgument::Value(copy(0, map_int)),
                            CallArgument::Value(copy(1, Type::Text)),
                        ],
                        witnesses: Vec::new(),
                    },
                    ty: Type::Int,
                    span: span(),
                })),
                span: span(),
            },
        )],
        prelude: PreludeIds {
            text_map: Some(map),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::BuiltinShape));
}

#[test]
fn text_map_entry_at_has_an_explicit_interpreted_artifact_encoding() {
    let encoded = serde_json::to_string(&loom_mir::Builtin::TextMapEntryAt)
        .expect("encode TextMap.entry_at builtin");
    assert_eq!(encoded, r#""text_map_entry_at""#);
    let decoded = serde_json::from_str::<loom_mir::Builtin>(&encoded)
        .expect("decode TextMap.entry_at builtin");
    assert_eq!(decoded, loom_mir::Builtin::TextMapEntryAt);
}

#[test]
fn list_to_text_map_rejects_a_non_tuple_list_at_the_checked_boundary() {
    let list = Type::List(Box::new(Type::Int));
    let program = Program {
        functions: vec![function(
            0,
            vec![local(0, list.clone(), false)],
            Vec::new(),
            Type::Int,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Call {
                        target: CallTarget::Builtin(loom_mir::Builtin::ListToTextMap),
                        type_arguments: Vec::new(),
                        arguments: vec![CallArgument::Value(copy(0, list))],
                        witnesses: Vec::new(),
                    },
                    ty: Type::Int,
                    span: span(),
                })),
                span: span(),
            },
        )],
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::BuiltinShape));
}

#[test]
fn list_to_text_map_has_an_explicit_interpreted_artifact_encoding() {
    let encoded = serde_json::to_string(&loom_mir::Builtin::ListToTextMap)
        .expect("encode List.to_text_map builtin");
    assert_eq!(encoded, r#""list_to_text_map""#);
    let decoded = serde_json::from_str::<loom_mir::Builtin>(&encoded)
        .expect("decode List.to_text_map builtin");
    assert_eq!(decoded, loom_mir::Builtin::ListToTextMap);
}

#[test]
fn text_map_equality_requires_value_equality_at_the_checked_boundary() {
    let map = TypeId(0);
    let file = TypeId(1);
    let file_ty = Type::Nominal(file, Vec::new());
    let map_file = Type::Nominal(map, vec![file_ty]);
    let program = Program {
        types: vec![
            TypeDef {
                id: map,
                name: "TextMap".to_owned(),
                span: span(),
                type_parameters: 1,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "raw".to_owned(),
                        ty: Type::Int,
                        span: span(),
                    }],
                    invariant: None,
                },
            },
            TypeDef {
                id: file,
                name: "File".to_owned(),
                span: span(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "raw".to_owned(),
                        ty: Type::Int,
                        span: span(),
                    }],
                    invariant: None,
                },
            },
        ],
        functions: vec![function(
            0,
            vec![
                local(0, map_file.clone(), false),
                local(1, map_file.clone(), false),
            ],
            Vec::new(),
            Type::Bool,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Binary(
                        loom_mir::BinaryOp::Equal,
                        Box::new(copy(0, map_file.clone())),
                        Box::new(copy(1, map_file)),
                    ),
                    ty: Type::Bool,
                    span: span(),
                })),
                span: span(),
            },
        )],
        prelude: PreludeIds {
            text_map: Some(map),
            file: Some(file),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::ExpressionShape));
}

#[test]
fn structured_log_write_requires_text_fields_at_the_checked_boundary() {
    let map = TypeId(0);
    let level = TypeId(1);
    let map_int = Type::Nominal(map, vec![Type::Int]);
    let level_ty = Type::Nominal(level, Vec::new());
    let program = Program {
        types: vec![
            TypeDef {
                id: map,
                name: "TextMap".to_owned(),
                span: span(),
                type_parameters: 1,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "raw".to_owned(),
                        ty: Type::Int,
                        span: span(),
                    }],
                    invariant: None,
                },
            },
            TypeDef {
                id: level,
                name: "LogLevel".to_owned(),
                span: span(),
                type_parameters: 0,
                kind: TypeDefKind::Enum {
                    variants: ["Debug", "Info", "Warn", "Error"]
                        .into_iter()
                        .enumerate()
                        .map(|(index, name)| VariantDef {
                            id: VariantId(u32::try_from(index).unwrap()),
                            name: name.to_owned(),
                            payload: Vec::new(),
                            span: span(),
                        })
                        .collect(),
                },
            },
        ],
        functions: vec![function(
            0,
            vec![
                local(0, level_ty.clone(), false),
                local(1, Type::Text, false),
                local(2, map_int.clone(), false),
            ],
            Vec::new(),
            Type::Unit,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Call {
                        target: CallTarget::Builtin(loom_mir::Builtin::LogWrite),
                        type_arguments: Vec::new(),
                        arguments: vec![
                            CallArgument::Value(copy(0, level_ty)),
                            CallArgument::Value(copy(1, Type::Text)),
                            CallArgument::Value(copy(2, map_int)),
                        ],
                        witnesses: Vec::new(),
                    },
                    ty: Type::Unit,
                    span: span(),
                })),
                span: span(),
            },
        )],
        prelude: PreludeIds {
            text_map: Some(map),
            log_level: Some(level),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::BuiltinShape));
}

#[test]
fn opaque_resource_prelude_shapes_are_checked() {
    let mut program = Program {
        types: vec![TypeDef {
            id: TypeId(0),
            name: "File".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "raw".to_owned(),
                    ty: Type::Text,
                    span: span(),
                }],
                invariant: None,
            },
        }],
        prelude: PreludeIds {
            file: Some(TypeId(0)),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::RecordShape));
    let TypeDefKind::Record { fields, .. } = &mut program.types[0].kind else {
        unreachable!();
    };
    fields[0].ty = Type::Int;
    validate_program(&program).expect("canonical opaque resource shape");
}

#[test]
fn canonical_prelude_type_roles_must_have_distinct_identities() {
    let int_backed = TypeId(0);
    let text_backed = TypeId(1);
    let program = Program {
        types: vec![
            raw_handle_type(int_backed.0, "IntBacked"),
            TypeDef {
                id: text_backed,
                name: "TextBacked".to_owned(),
                span: span(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "raw".to_owned(),
                        ty: Type::Text,
                        span: span(),
                    }],
                    invariant: None,
                },
            },
        ],
        prelude: PreludeIds {
            duration: Some(int_backed),
            file: Some(int_backed),
            socket: Some(int_backed),
            bytes: Some(text_backed),
            path: Some(text_backed),
            ..PreludeIds::default()
        },
        ..Program::default()
    };

    let errors = validation_errors(&program);
    for (path, first) in [
        ("prelude.file", "duration"),
        ("prelude.socket", "duration"),
        ("prelude.path", "bytes"),
    ] {
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code == MirValidationCode::InvalidTypeReference
                    && error.path == path
                    && error.message.contains(first)
                    && error.message.contains("distinct type identity")
            }),
            "missing alias rejection for {path}: {errors:#?}"
        );
    }
}

fn io_error_types() -> Vec<TypeDef> {
    let kind = TypeId(0);
    vec![
        TypeDef {
            id: kind,
            name: "IoErrorKind".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Enum {
                variants: [
                    "NotFound",
                    "PermissionDenied",
                    "AlreadyExists",
                    "InvalidInput",
                    "ConnectionRefused",
                    "ConnectionReset",
                    "TimedOut",
                    "UnexpectedEof",
                    "Closed",
                    "Other",
                ]
                .into_iter()
                .enumerate()
                .map(|(index, name)| VariantDef {
                    id: VariantId(u32::try_from(index).expect("I/O error kind index")),
                    name: name.to_owned(),
                    payload: Vec::new(),
                    span: span(),
                })
                .collect(),
            },
        },
        TypeDef {
            id: TypeId(1),
            name: "IoError".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![
                    FieldDef {
                        name: "kind".to_owned(),
                        ty: Type::Nominal(kind, Vec::new()),
                        span: span(),
                    },
                    FieldDef {
                        name: "message".to_owned(),
                        ty: Type::Text,
                        span: span(),
                    },
                ],
                invariant: None,
            },
        },
    ]
}

#[test]
fn prelude_io_error_allows_only_its_checked_accessors() {
    let kind = TypeId(0);
    let error = TypeId(1);
    let error_ty = Type::Nominal(error, Vec::new());
    let kind_ty = Type::Nominal(kind, Vec::new());
    let accessor = |builtin, ty| {
        expr(
            ExprKind::Call {
                target: CallTarget::Builtin(builtin),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::Value(copy(0, error_ty.clone()))],
                witnesses: Vec::new(),
            },
            ty,
        )
    };
    let program = Program {
        types: io_error_types(),
        functions: vec![function(
            0,
            vec![local(0, error_ty.clone(), false)],
            Vec::new(),
            Type::Text,
            Block {
                statements: vec![Statement {
                    kind: StatementKind::Evaluate(accessor(
                        loom_mir::Builtin::IoErrorKind,
                        kind_ty,
                    )),
                    span: span(),
                }],
                tail: Some(Box::new(accessor(
                    loom_mir::Builtin::IoErrorMessage,
                    Type::Text,
                ))),
                span: span(),
            },
        )],
        prelude: PreludeIds {
            io_error: Some(error),
            io_error_kind: Some(kind),
            ..PreludeIds::default()
        },
        ..Program::default()
    };

    validate_program(&program).expect("IoError accessors are valid checked MIR observers");
}

#[test]
fn prelude_io_error_cannot_be_forged_or_projected_in_checked_mir() {
    let kind = TypeId(0);
    let error = TypeId(1);
    let kind_ty = Type::Nominal(kind, Vec::new());
    let error_ty = Type::Nominal(error, Vec::new());
    let forged = expr(
        ExprKind::Record {
            ty: error,
            type_arguments: Vec::new(),
            fields: vec![
                expr(
                    ExprKind::Variant {
                        ty: kind,
                        type_arguments: Vec::new(),
                        variant: VariantId(9),
                        payload: Vec::new(),
                    },
                    kind_ty.clone(),
                ),
                constant(Constant::Text("forged".to_owned()), Type::Text),
            ],
            construction: ConstructionMode::Plain,
        },
        error_ty.clone(),
    );
    let kind_projection = Place {
        local: LocalId(0),
        projection: vec![0],
    };
    let program = Program {
        types: io_error_types(),
        functions: vec![function(
            0,
            vec![local(0, error_ty, false)],
            Vec::new(),
            kind_ty.clone(),
            Block {
                statements: vec![Statement {
                    kind: StatementKind::Evaluate(forged),
                    span: span(),
                }],
                tail: Some(Box::new(copy_place(kind_projection, kind_ty))),
                span: span(),
            },
        )],
        prelude: PreludeIds {
            io_error: Some(error),
            io_error_kind: Some(kind),
            ..PreludeIds::default()
        },
        ..Program::default()
    };

    let errors = validation_errors(&program);
    for (code, message) in [
        (
            MirValidationCode::RecordShape,
            "IoError values may only be established",
        ),
        (
            MirValidationCode::InvalidPlace,
            "IoError storage is protected",
        ),
    ] {
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code == code && error.message.contains(message)),
            "missing {code:?} `{message}`: {errors:#?}"
        );
    }
}

#[test]
fn prelude_path_cannot_be_forged_or_projected_in_checked_mir() {
    let path = TypeId(0);
    let path_ty = Type::Nominal(path, Vec::new());
    let forged = expr(
        ExprKind::Record {
            ty: path,
            type_arguments: Vec::new(),
            fields: vec![constant(Constant::Text("bad\0path".to_owned()), Type::Text)],
            construction: ConstructionMode::Plain,
        },
        path_ty.clone(),
    );
    let raw = Place {
        local: LocalId(0),
        projection: vec![0],
    };
    let program = Program {
        types: vec![TypeDef {
            id: path,
            name: "Path".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "raw".to_owned(),
                    ty: Type::Text,
                    span: span(),
                }],
                invariant: None,
            },
        }],
        functions: vec![function(
            0,
            vec![local(0, path_ty.clone(), false)],
            vec![local(1, path_ty, false)],
            Type::Text,
            Block {
                statements: vec![Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: forged,
                    },
                    span: span(),
                }],
                tail: Some(Box::new(copy_place(raw, Type::Text))),
                span: span(),
            },
        )],
        prelude: PreludeIds {
            path: Some(path),
            ..PreludeIds::default()
        },
        ..Program::default()
    };

    let errors = validation_errors(&program);
    for (code, message) in [
        (
            MirValidationCode::RecordShape,
            "Path values may only be established",
        ),
        (MirValidationCode::InvalidPlace, "Path storage is opaque"),
    ] {
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code == code && error.message.contains(message)),
            "missing {code:?} `{message}`: {errors:#?}"
        );
    }
}

#[test]
fn canonical_resources_cannot_be_forged_or_projected_in_checked_mir() {
    for (name, file) in [("File", true), ("Socket", false)] {
        let resource = TypeId(0);
        let resource_ty = Type::Nominal(resource, Vec::new());
        let forged = expr(
            ExprKind::Record {
                ty: resource,
                type_arguments: Vec::new(),
                fields: vec![constant(Constant::Int(41), Type::Int)],
                construction: ConstructionMode::Plain,
            },
            resource_ty.clone(),
        );
        let raw = Place {
            local: LocalId(0),
            projection: vec![0],
        };
        let mut prelude = PreludeIds::default();
        if file {
            prelude.file = Some(resource);
        } else {
            prelude.socket = Some(resource);
        }
        let program = Program {
            types: vec![raw_handle_type(resource.0, name)],
            functions: vec![function(
                0,
                vec![local(0, resource_ty.clone(), false)],
                vec![local(1, resource_ty, false)],
                Type::Int,
                Block {
                    statements: vec![Statement {
                        kind: StatementKind::Let {
                            local: LocalId(1),
                            value: forged,
                        },
                        span: span(),
                    }],
                    tail: Some(Box::new(copy_place(raw, Type::Int))),
                    span: span(),
                },
            )],
            prelude,
            ..Program::default()
        };

        let errors = validation_errors(&program);
        for (code, message) in [
            (
                MirValidationCode::RecordShape,
                format!("{name} values may only be established"),
            ),
            (
                MirValidationCode::InvalidPlace,
                format!("{name} storage is protected"),
            ),
        ] {
            assert!(
                errors
                    .as_slice()
                    .iter()
                    .any(|error| error.code == code && error.message.contains(&message)),
                "missing {code:?} `{message}`: {errors:#?}"
            );
        }
    }
}

#[test]
fn resource_close_requires_an_inout_place() {
    let file = TypeId(0);
    let file_ty = Type::Nominal(file, Vec::new());
    let mut program = Program {
        types: vec![TypeDef {
            id: file,
            name: "File".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "raw".to_owned(),
                    ty: Type::Int,
                    span: span(),
                }],
                invariant: None,
            },
        }],
        functions: vec![function(
            0,
            vec![local(0, file_ty.clone(), true)],
            Vec::new(),
            Type::Unit,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Call {
                        target: CallTarget::Builtin(loom_mir::Builtin::FileClose),
                        type_arguments: Vec::new(),
                        arguments: vec![CallArgument::Value(copy(0, file_ty))],
                        witnesses: Vec::new(),
                    },
                    ty: Type::Unit,
                    span: span(),
                })),
                span: span(),
            },
        )],
        prelude: PreludeIds {
            file: Some(file),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::ReceiverShape));
    let Some(tail) = &mut program.functions[0].body.tail else {
        unreachable!();
    };
    let ExprKind::Call { arguments, .. } = &mut tail.kind else {
        unreachable!();
    };
    arguments[0] = CallArgument::InOut(Place::local(LocalId(0)));
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::ObligationShape));
}

#[test]
fn only_the_exact_canonical_dispose_witness_may_close_a_resource() {
    for (name, builtin, file) in [
        ("File", loom_mir::Builtin::FileClose, true),
        ("Socket", loom_mir::Builtin::SocketClose, false),
    ] {
        let main = function(
            1,
            Vec::new(),
            Vec::new(),
            Type::Unit,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        );
        let mut program = resource_program(main, Vec::new(), false);
        program.types[0].name = name.to_owned();
        if file {
            program.prelude.file = Some(TypeId(0));
        } else {
            program.prelude.socket = Some(TypeId(0));
        }
        program.functions[0].body.tail = Some(Box::new(expr(
            ExprKind::Call {
                target: CallTarget::Builtin(builtin),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::InOut(Place::local(LocalId(0)))],
                witnesses: Vec::new(),
            },
            Type::Unit,
        )));
        program.functions[0]
            .renumber_expr_ids()
            .expect("dispose expression ids");
        validate_program(&program).unwrap_or_else(|errors| {
            panic!("canonical {name} Dispose witness must own close authority: {errors:#?}")
        });

        let mut mismatched = program.clone();
        let (other_name, other_builtin) = if file {
            mismatched.prelude.socket = Some(TypeId(1));
            ("Socket", loom_mir::Builtin::SocketClose)
        } else {
            mismatched.prelude.file = Some(TypeId(1));
            ("File", loom_mir::Builtin::FileClose)
        };
        mismatched.types.push(raw_handle_type(1, other_name));
        let Some(tail) = &mut mismatched.functions[0].body.tail else {
            unreachable!();
        };
        let ExprKind::Call { target, .. } = &mut tail.kind else {
            unreachable!();
        };
        *target = CallTarget::Builtin(other_builtin);
        let errors = validation_errors(&mismatched);
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code == MirValidationCode::ObligationShape
                    && error.path == "functions[0].body.tail.target"
                    && error
                        .message
                        .contains("matching canonical Dispose implementation")
            }),
            "a canonical {name} Dispose witness must not close {other_name}: {errors:#?}"
        );

        let mut helper = program.functions[0].clone();
        helper.id = FunctionId(2);
        helper.name = format!("unauthorized_{name}_close");
        program.functions.push(helper);
        let errors = validation_errors(&program);
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code == MirValidationCode::ObligationShape
                    && error.path == "functions[2].body.tail.target"
                    && error
                        .message
                        .contains("matching canonical Dispose implementation")
            }),
            "a same-signature helper must not inherit {name} close authority: {errors:#?}"
        );
    }
}

#[test]
fn scoped_statement_and_canonical_resource_identity_round_trips_in_current_artifact() {
    let main = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::Unit,
        Block {
            statements: vec![scoped_resource(0, 7)],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let checked = resource_program(main, Vec::new(), false)
        .into_checked()
        .expect("canonical scoped MIR");
    let encoded = encode_interpreted_artifact(&checked).expect("encode scoped artifact");
    let decoded = decode_interpreted_artifact(&encoded).expect("decode scoped artifact");
    assert_eq!(decoded.prelude.dispose_concept, Some(ConceptId(0)));
    assert_eq!(decoded.prelude.must_scope_concept, Some(ConceptId(1)));
    assert_eq!(decoded.concepts[0].identity, Some(ConceptIdentity::Dispose));
    assert_eq!(
        decoded.concepts[1].identity,
        Some(ConceptIdentity::MustScope)
    );
    assert_eq!(
        decoded.concepts[2].identity,
        Some(ConceptIdentity::NoSuspend)
    );
    assert!(matches!(
        decoded.functions[1].body.statements[0].kind,
        StatementKind::Scoped {
            disposal: ScopedDisposal::StaticConcept { .. },
            ..
        }
    ));
}

#[test]
fn interpreted_artifact_boundaries_require_the_complete_resource_identity_trio() {
    let low_level = Program::default()
        .into_checked()
        .expect("low-level checked MIR may omit compiler artifact metadata");
    let encode_error = encode_interpreted_artifact(&low_level)
        .expect_err("production artifact encoding requires all resource identities");
    assert!(
        matches!(encode_error, ArtifactError::InvalidProgram(ref errors) if errors.contains(MirValidationCode::ConceptShape)),
        "{encode_error:?}"
    );

    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits()))
        .expect("complete artifact fixture");
    let mut absent: serde_json::Value = serde_json::from_slice(&bytes).expect("artifact JSON");
    absent["program"]["concepts"] = serde_json::json!([]);
    absent["program"]["requirements"] = serde_json::json!([]);
    for field in [
        "dispose_concept",
        "dispose_requirement",
        "must_scope_concept",
        "no_suspend_concept",
    ] {
        absent["program"]["prelude"][field] = serde_json::Value::Null;
    }
    let decode_error = decode_interpreted_artifact(
        &serde_json::to_vec(&absent).expect("encode absent resource metadata"),
    )
    .expect_err("production artifact decoding requires all resource identities");
    assert!(
        matches!(decode_error, ArtifactError::InvalidProgram(ref errors) if errors.contains(MirValidationCode::ConceptShape)),
        "{decode_error:?}"
    );
}

#[test]
fn forged_must_scope_identities_fail_closed_at_artifact_decode() {
    let main = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::Unit,
        Block {
            statements: vec![scoped_resource(0, 7)],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let mut program = resource_program(main, Vec::new(), false);
    program.concepts.push(ConceptDef {
        id: ConceptId(3),
        module: "forged.decoy".to_owned(),
        name: "Decoy".to_owned(),
        span: span(),
        identity: None,
        dynamic: false,
        associated_types: Vec::new(),
        requirements: Vec::new(),
    });
    let checked = program.into_checked().expect("resource identity fixture");
    let bytes = encode_interpreted_artifact(&checked).expect("encode resource identity fixture");
    let original: serde_json::Value = serde_json::from_slice(&bytes).expect("artifact JSON");

    let mut omitted_field = original.clone();
    omitted_field["program"]["concepts"][1]
        .as_object_mut()
        .expect("concept object")
        .remove("identity");
    let error = decode_interpreted_artifact(
        &serde_json::to_vec(&omitted_field).expect("encode missing-field artifact"),
    )
    .expect_err("a current concept cannot omit its identity field");
    assert!(
        matches!(error, ArtifactError::Malformed(ref message) if message.contains("missing field `identity`")),
        "{error:?}"
    );

    let rejects_identity = |label: &str, value: &serde_json::Value| {
        let error = decode_interpreted_artifact(
            &serde_json::to_vec(value).expect("encode forged artifact JSON"),
        )
        .unwrap_err();
        assert!(
            matches!(error, ArtifactError::InvalidProgram(ref errors) if errors.contains(MirValidationCode::ConceptShape)),
            "{label}: {error:?}"
        );
    };

    let mut missing_tag = original.clone();
    missing_tag["program"]["concepts"][1]["identity"] = serde_json::Value::Null;
    rejects_identity("missing tag", &missing_tag);

    let mut missing_prelude = original.clone();
    missing_prelude["program"]["prelude"]["must_scope_concept"] = serde_json::Value::Null;
    rejects_identity("missing prelude id", &missing_prelude);

    let mut removed_both = original.clone();
    removed_both["program"]["concepts"][1]["identity"] = serde_json::Value::Null;
    removed_both["program"]["prelude"]["must_scope_concept"] = serde_json::Value::Null;
    rejects_identity("removed tag and prelude id", &removed_both);

    let mut redirected = original.clone();
    redirected["program"]["prelude"]["must_scope_concept"] = serde_json::json!(3);
    rejects_identity("redirected prelude id", &redirected);

    let mut duplicate = original.clone();
    duplicate["program"]["concepts"][3]["identity"] = serde_json::json!("mustScope");
    rejects_identity("duplicate tag", &duplicate);

    let mut wrong_name = original.clone();
    wrong_name["program"]["concepts"][1]["name"] = serde_json::json!("ForgedScope");
    rejects_identity("wrong canonical name", &wrong_name);

    let mut wrong_shape = original;
    wrong_shape["program"]["concepts"][1]["dynamic"] = serde_json::json!(true);
    rejects_identity("invalid marker shape", &wrong_shape);
}

#[test]
fn dispose_and_no_suspend_reject_forged_tags_names_and_modules_at_artifact_decode() {
    let main = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::Unit,
        Block {
            statements: vec![scoped_resource(0, 7)],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let checked = resource_program(main, Vec::new(), true)
        .into_checked()
        .expect("resource provenance fixture");
    let bytes = encode_interpreted_artifact(&checked).expect("encode resource provenance fixture");
    let original: serde_json::Value = serde_json::from_slice(&bytes).expect("artifact JSON");

    let rejects_identity = |label: &str, value: &serde_json::Value| {
        let error = decode_interpreted_artifact(
            &serde_json::to_vec(value).expect("encode forged artifact JSON"),
        )
        .expect_err("forged resource language-item identity must be rejected");
        assert!(
            matches!(error, ArtifactError::InvalidProgram(ref errors) if errors.contains(MirValidationCode::ConceptShape)),
            "{label}: {error:?}"
        );
    };

    for (label, index, forged_tag) in [("Dispose", 0, "noSuspend"), ("NoSuspend", 2, "dispose")] {
        let mut wrong_module = original.clone();
        wrong_module["program"]["concepts"][index]["module"] =
            serde_json::json!("application.resource");
        rejects_identity(&format!("{label} wrong module"), &wrong_module);

        let mut wrong_name = original.clone();
        wrong_name["program"]["concepts"][index]["name"] =
            serde_json::json!(format!("Forged{label}"));
        rejects_identity(&format!("{label} wrong name"), &wrong_name);

        let mut missing_tag = original.clone();
        missing_tag["program"]["concepts"][index]["identity"] = serde_json::Value::Null;
        rejects_identity(&format!("{label} missing tag"), &missing_tag);

        let mut wrong_tag = original.clone();
        wrong_tag["program"]["concepts"][index]["identity"] = serde_json::json!(forged_tag);
        rejects_identity(&format!("{label} wrong tag"), &wrong_tag);
    }
}

#[test]
fn source_spelling_without_a_compiler_identity_tag_has_no_resource_semantics() {
    for module in ["application", "std.resource"] {
        let concept = ConceptDef {
            id: ConceptId(0),
            module: module.to_owned(),
            name: "MustScope".to_owned(),
            span: span(),
            identity: None,
            dynamic: false,
            associated_types: Vec::new(),
            requirements: Vec::new(),
        };
        let witness = Witness {
            id: WitnessId(0),
            concept: ConceptId(0),
            concrete: Type::Int,
            methods: BTreeMap::new(),
            associated: BTreeMap::new(),
            type_parameters: 0,
            prerequisites: Vec::new(),
        };
        let returns_int = function(
            0,
            Vec::new(),
            Vec::new(),
            Type::Int,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(constant(Constant::Int(1), Type::Int))),
                span: span(),
            },
        );
        validate_program(&Program {
            concepts: vec![concept],
            functions: vec![returns_int],
            witnesses: vec![witness],
            ..Program::default()
        })
        .unwrap_or_else(|errors| panic!("{module} spelling acquired resource authority: {errors}"));
    }
}

#[test]
fn forged_resource_marker_prelude_ids_fail_closed() {
    let main = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::Unit,
        Block {
            statements: vec![scoped_resource(0, 1)],
            tail: None,
            span: span(),
        },
    );
    let mut program = resource_program(main, Vec::new(), false);
    program.prelude.must_scope_concept = Some(ConceptId(0));
    assert!(validation_errors(&program).contains(MirValidationCode::ConceptShape));

    program.prelude.must_scope_concept = Some(ConceptId(1));
    program.prelude.dispose_requirement = None;
    assert!(validation_errors(&program).contains(MirValidationCode::ConceptShape));
}

#[test]
#[allow(clippy::too_many_lines)]
fn portable_mir_rejects_every_must_scope_escape_surface() {
    let ordinary_let = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), false)],
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Let {
                    local: LocalId(0),
                    value: resource_value(1),
                },
                span: span(),
            }],
            tail: None,
            span: span(),
        },
    );
    let discarded = function(
        1,
        Vec::new(),
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(resource_value(2)),
                span: span(),
            }],
            tail: None,
            span: span(),
        },
    );
    let copied = function(
        1,
        vec![local(0, resource_type(), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(copy(0, resource_type())),
                span: span(),
            }],
            tail: None,
            span: span(),
        },
    );
    let returned = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        resource_type(),
        Block {
            statements: vec![
                scoped_resource(0, 3),
                Statement {
                    kind: StatementKind::Return(Some(copy(0, resource_type()))),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    let container = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::Unit,
        Block {
            statements: vec![
                scoped_resource(0, 4),
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Tuple(vec![copy(0, resource_type())]),
                        Type::Tuple(vec![resource_type()]),
                    )),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    for (name, main) in [
        ("ordinary Let", ordinary_let),
        ("discard", discarded),
        ("copy", copied),
        ("return", returned),
        ("container", container),
    ] {
        let errors = validation_errors(&resource_program(main, Vec::new(), false));
        assert!(
            errors.contains(MirValidationCode::ObligationShape),
            "{name}: {errors:?}"
        );
    }

    let sink = function(
        2,
        vec![local(0, resource_type(), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: None,
            span: span(),
        },
    );
    let ordinary_argument = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::Unit,
        Block {
            statements: vec![
                scoped_resource(0, 5),
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Call {
                            target: CallTarget::Direct(FunctionId(2)),
                            type_arguments: Vec::new(),
                            arguments: vec![CallArgument::Value(copy(0, resource_type()))],
                            witnesses: Vec::new(),
                        },
                        Type::Unit,
                    )),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    let errors = validation_errors(&resource_program(ordinary_argument, vec![sink], false));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ObligationShape
            && error.message.contains("ordinary argument")
    }));

    let manual_dispose = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::Unit,
        Block {
            statements: vec![
                scoped_resource(0, 6),
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Call {
                            target: CallTarget::StaticConcept {
                                requirement: RequirementId(0),
                                witness: WitnessRef::Concrete(WitnessId(0)),
                                dispatch_type: resource_type(),
                            },
                            type_arguments: Vec::new(),
                            arguments: vec![CallArgument::InOut(Place::local(LocalId(0)))],
                            witnesses: Vec::new(),
                        },
                        Type::Unit,
                    )),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    let errors = validation_errors(&resource_program(manual_dispose, Vec::new(), false));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ObligationShape
            && error.message.contains("only be invoked by a Scoped")
    }));
}

#[test]
#[allow(clippy::too_many_lines)]
fn portable_mir_resource_exemptions_apply_only_to_the_exact_expression_root() {
    let nested_evaluate = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::Unit,
        Block {
            statements: vec![
                scoped_resource(0, 10),
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Binary(
                            BinaryOp::Equal,
                            Box::new(copy(0, resource_type())),
                            Box::new(copy(0, resource_type())),
                        ),
                        Type::Bool,
                    )),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    let errors = validation_errors(&resource_program(nested_evaluate, Vec::new(), false));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ObligationShape && error.path.contains("expression.left")
    }));

    let nested_initializer = function(
        1,
        Vec::new(),
        vec![
            local(0, resource_type(), true),
            local(1, resource_type(), true),
        ],
        Type::Unit,
        Block {
            statements: vec![
                scoped_resource(0, 11),
                Statement {
                    kind: StatementKind::Scoped {
                        local: LocalId(1),
                        value: expr(
                            ExprKind::Block(Block {
                                statements: vec![
                                    Statement {
                                        kind: StatementKind::Evaluate(expr(
                                            ExprKind::Call {
                                                target: CallTarget::Direct(FunctionId(2)),
                                                type_arguments: Vec::new(),
                                                arguments: vec![CallArgument::Value(copy(
                                                    0,
                                                    resource_type(),
                                                ))],
                                                witnesses: Vec::new(),
                                            },
                                            Type::Unit,
                                        )),
                                        span: span(),
                                    },
                                    Statement {
                                        kind: StatementKind::Evaluate(expr(
                                            ExprKind::Tuple(vec![copy(0, resource_type())]),
                                            Type::Tuple(vec![resource_type()]),
                                        )),
                                        span: span(),
                                    },
                                ],
                                tail: Some(Box::new(expr(
                                    ExprKind::If {
                                        condition: Box::new(expr(
                                            ExprKind::Binary(
                                                BinaryOp::Equal,
                                                Box::new(copy(0, resource_type())),
                                                Box::new(copy(0, resource_type())),
                                            ),
                                            Type::Bool,
                                        )),
                                        then_branch: Block {
                                            statements: Vec::new(),
                                            tail: Some(Box::new(resource_value(12))),
                                            span: span(),
                                        },
                                        else_branch: Block {
                                            statements: Vec::new(),
                                            tail: Some(Box::new(resource_value(13))),
                                            span: span(),
                                        },
                                    },
                                    resource_type(),
                                ))),
                                span: span(),
                            }),
                            resource_type(),
                        ),
                        disposal: ScopedDisposal::StaticConcept {
                            requirement: RequirementId(0),
                            witness: WitnessRef::Concrete(WitnessId(0)),
                            dispatch_type: resource_type(),
                        },
                    },
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    let sink = function(
        2,
        vec![local(0, resource_type(), false)],
        vec![local(1, resource_type(), true)],
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Scoped {
                    local: LocalId(1),
                    value: copy(0, resource_type()),
                    disposal: ScopedDisposal::StaticConcept {
                        requirement: RequirementId(0),
                        witness: WitnessRef::Concrete(WitnessId(0)),
                        dispatch_type: resource_type(),
                    },
                },
                span: span(),
            }],
            tail: None,
            span: span(),
        },
    );
    let errors = validation_errors(&resource_program(nested_initializer, vec![sink], false));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ObligationShape
            && error
                .path
                .contains("value.block.statements[0].expression.arguments[0]")
    }));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ObligationShape
            && error
                .path
                .contains("value.block.statements[1].expression.elements[0]")
    }));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ObligationShape
            && error.path.contains("value.block.tail.condition.left")
    }));
}

#[test]
#[allow(clippy::too_many_lines)]
fn portable_mir_allows_match_to_transfer_one_resource_into_scoped() {
    let carrier = TypeId(1);
    let carrier_ty = Type::Nominal(carrier, Vec::new());
    let resource_binding = LocalId(2);
    let matched_resource = expr(
        ExprKind::Match {
            scrutinee: Box::new(move_local(0, carrier_ty.clone())),
            arms: vec![
                MatchArm {
                    pattern: Pattern::Variant {
                        ty: carrier,
                        variant: VariantId(0),
                        payload: vec![Pattern::Binding],
                    },
                    bindings: vec![resource_binding],
                    value: move_local(resource_binding.0, resource_type()),
                },
                MatchArm {
                    pattern: Pattern::Variant {
                        ty: carrier,
                        variant: VariantId(1),
                        payload: Vec::new(),
                    },
                    bindings: Vec::new(),
                    value: resource_value(13),
                },
            ],
        },
        resource_type(),
    );
    let main = function(
        1,
        vec![local(0, carrier_ty, false)],
        vec![
            local(1, resource_type(), true),
            local(resource_binding.0, resource_type(), false),
        ],
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Scoped {
                    local: LocalId(1),
                    value: matched_resource,
                    disposal: ScopedDisposal::StaticConcept {
                        requirement: RequirementId(0),
                        witness: WitnessRef::Concrete(WitnessId(0)),
                        dispatch_type: resource_type(),
                    },
                },
                span: span(),
            }],
            tail: None,
            span: span(),
        },
    );
    let mut program = resource_program(main, Vec::new(), false);
    program.types.push(TypeDef {
        id: carrier,
        name: "ResourceResult".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Enum {
            variants: vec![
                VariantDef {
                    id: VariantId(0),
                    name: "Ok".to_owned(),
                    payload: vec![resource_type()],
                    span: span(),
                },
                VariantDef {
                    id: VariantId(1),
                    name: "Fallback".to_owned(),
                    payload: Vec::new(),
                    span: span(),
                },
            ],
        },
    });

    validate_program(&program).expect("a match may transfer exactly one arm root into Scoped");

    let StatementKind::Scoped { value, .. } = &mut program.functions[1].body.statements[0].kind
    else {
        panic!("test fixture must contain Scoped");
    };
    let ExprKind::Match { arms, .. } = &mut value.kind else {
        panic!("test fixture must contain a match initializer");
    };
    arms[0].value = resource_value(14);
    program.functions[1]
        .renumber_expr_ids()
        .expect("renumber hostile match fixture");
    let errors = validation_errors(&program);
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ObligationShape
            && error.path.ends_with("value.arms[0].bindings[0]")
            && error.message.contains("transfer directly into Scoped")
    }));
}

#[test]
#[allow(clippy::too_many_lines)]
fn portable_mir_match_consumes_resource_carriers_but_not_active_scoped_values() {
    let carrier = TypeId(1);
    let carrier_ty = Type::Nominal(carrier, Vec::new());
    let binding = LocalId(1);
    let scoped = LocalId(2);
    let consume_carrier = function(
        1,
        vec![local(0, carrier_ty.clone(), false)],
        vec![
            local(binding.0, resource_type(), false),
            local(scoped.0, resource_type(), true),
        ],
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(expr(
                    ExprKind::Match {
                        scrutinee: Box::new(move_local(0, carrier_ty.clone())),
                        arms: vec![
                            MatchArm {
                                pattern: Pattern::Variant {
                                    ty: carrier,
                                    variant: VariantId(0),
                                    payload: vec![Pattern::Binding],
                                },
                                bindings: vec![binding],
                                value: expr(
                                    ExprKind::Block(Block {
                                        statements: vec![Statement {
                                            kind: StatementKind::Scoped {
                                                local: scoped,
                                                value: move_local(binding.0, resource_type()),
                                                disposal: ScopedDisposal::StaticConcept {
                                                    requirement: RequirementId(0),
                                                    witness: WitnessRef::Concrete(WitnessId(0)),
                                                    dispatch_type: resource_type(),
                                                },
                                            },
                                            span: span(),
                                        }],
                                        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                                        span: span(),
                                    }),
                                    Type::Unit,
                                ),
                            },
                            MatchArm {
                                pattern: Pattern::Variant {
                                    ty: carrier,
                                    variant: VariantId(1),
                                    payload: Vec::new(),
                                },
                                bindings: Vec::new(),
                                value: constant(Constant::Unit, Type::Unit),
                            },
                        ],
                    },
                    Type::Unit,
                )),
                span: span(),
            }],
            tail: None,
            span: span(),
        },
    );
    let mut carrier_program = resource_program(consume_carrier, Vec::new(), false);
    carrier_program.types.push(TypeDef {
        id: carrier,
        name: "ResourceCarrier".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Enum {
            variants: vec![
                VariantDef {
                    id: VariantId(0),
                    name: "Resource".to_owned(),
                    payload: vec![resource_type()],
                    span: span(),
                },
                VariantDef {
                    id: VariantId(1),
                    name: "Empty".to_owned(),
                    payload: Vec::new(),
                    span: span(),
                },
            ],
        },
    });
    validate_program(&carrier_program)
        .expect("match arms may transfer each resource payload into their own Scoped binding");

    let active = LocalId(0);
    let rebound = LocalId(1);
    let rescoped = LocalId(2);
    let match_active = function(
        1,
        Vec::new(),
        vec![
            local(active.0, resource_type(), true),
            local(rebound.0, resource_type(), false),
            local(rescoped.0, resource_type(), true),
        ],
        Type::Unit,
        Block {
            statements: vec![
                scoped_resource(active.0, 15),
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Match {
                            scrutinee: Box::new(copy(active.0, resource_type())),
                            arms: vec![MatchArm {
                                pattern: Pattern::Binding,
                                bindings: vec![rebound],
                                value: expr(
                                    ExprKind::Block(Block {
                                        statements: vec![Statement {
                                            kind: StatementKind::Scoped {
                                                local: rescoped,
                                                value: move_local(rebound.0, resource_type()),
                                                disposal: ScopedDisposal::StaticConcept {
                                                    requirement: RequirementId(0),
                                                    witness: WitnessRef::Concrete(WitnessId(0)),
                                                    dispatch_type: resource_type(),
                                                },
                                            },
                                            span: span(),
                                        }],
                                        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                                        span: span(),
                                    }),
                                    Type::Unit,
                                ),
                            }],
                        },
                        Type::Unit,
                    )),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    let errors = validation_errors(&resource_program(match_active, Vec::new(), false));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ObligationShape
            && error
                .message
                .contains("active scoped resource cannot be consumed")
    }));
}

#[test]
#[allow(clippy::too_many_lines)]
fn portable_mir_rejects_move_based_resource_escape_surfaces() {
    let direct_return = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        resource_type(),
        Block {
            statements: vec![
                scoped_resource(0, 20),
                Statement {
                    kind: StatementKind::Return(Some(move_local(0, resource_type()))),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    let tail_return = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        resource_type(),
        Block {
            statements: vec![scoped_resource(0, 21)],
            tail: Some(Box::new(move_local(0, resource_type()))),
            span: span(),
        },
    );
    let tuple_return = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::Tuple(vec![resource_type()]),
        Block {
            statements: vec![scoped_resource(0, 22)],
            tail: Some(Box::new(expr(
                ExprKind::Tuple(vec![move_local(0, resource_type())]),
                Type::Tuple(vec![resource_type()]),
            ))),
            span: span(),
        },
    );
    let list_return = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::List(Box::new(resource_type())),
        Block {
            statements: vec![scoped_resource(0, 23)],
            tail: Some(Box::new(expr(
                ExprKind::List(vec![move_local(0, resource_type())]),
                Type::List(Box::new(resource_type())),
            ))),
            span: span(),
        },
    );
    let ordinary_let = function(
        1,
        Vec::new(),
        vec![
            local(0, resource_type(), true),
            local(1, resource_type(), false),
        ],
        Type::Unit,
        Block {
            statements: vec![
                scoped_resource(0, 24),
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: move_local(0, resource_type()),
                    },
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    for (name, main) in [
        ("direct Return", direct_return),
        ("tail return", tail_return),
        ("tuple return", tuple_return),
        ("list return", list_return),
        ("ordinary Let", ordinary_let),
    ] {
        let errors = validation_errors(&resource_program(main, Vec::new(), false));
        assert!(
            errors.contains(MirValidationCode::ObligationShape),
            "{name}: {errors:?}"
        );
    }

    let wrapper = TypeId(1);
    let mut record_program = resource_program(
        function(
            1,
            Vec::new(),
            vec![local(0, resource_type(), true)],
            Type::Nominal(wrapper, Vec::new()),
            Block {
                statements: vec![scoped_resource(0, 25)],
                tail: Some(Box::new(expr(
                    ExprKind::Record {
                        ty: wrapper,
                        type_arguments: Vec::new(),
                        fields: vec![move_local(0, resource_type())],
                        construction: ConstructionMode::Plain,
                    },
                    Type::Nominal(wrapper, Vec::new()),
                ))),
                span: span(),
            },
        ),
        Vec::new(),
        false,
    );
    record_program.types.push(TypeDef {
        id: wrapper,
        name: "Wrapper".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "resource".to_owned(),
                ty: resource_type(),
                span: span(),
            }],
            invariant: None,
        },
    });
    assert!(validation_errors(&record_program).contains(MirValidationCode::ObligationShape));
}

#[test]
#[allow(clippy::too_many_lines)]
fn portable_mir_rejects_fresh_must_scope_results_at_every_function_sink() {
    let returning = |value: Expr, explicit: bool| {
        let return_ty = value.ty.clone();
        let (statements, tail) = if explicit {
            (
                vec![Statement {
                    kind: StatementKind::Return(Some(value)),
                    span: span(),
                }],
                None,
            )
        } else {
            (Vec::new(), Some(Box::new(value)))
        };
        function(
            1,
            Vec::new(),
            Vec::new(),
            return_ty,
            Block {
                statements,
                tail,
                span: span(),
            },
        )
    };
    let assert_main_sink = |program: Program, expected_path: &str| {
        let errors = validation_errors(&program);
        assert!(
            errors.iter().any(|error| {
                error.code == MirValidationCode::ObligationShape
                    && error.path == expected_path
                    && error.message.contains("transfer it into Scoped")
            }),
            "{expected_path}: {errors:?}"
        );
    };

    assert_main_sink(
        resource_program(returning(resource_value(30), true), Vec::new(), false),
        "functions[1].body.statements[0].value",
    );
    assert_main_sink(
        resource_program(returning(resource_value(31), false), Vec::new(), false),
        "functions[1].body.tail",
    );

    for (name, value, explicit) in [
        (
            "tuple",
            expr(
                ExprKind::Tuple(vec![resource_value(32)]),
                Type::Tuple(vec![resource_type()]),
            ),
            true,
        ),
        (
            "list",
            expr(
                ExprKind::List(vec![resource_value(33)]),
                Type::List(Box::new(resource_type())),
            ),
            false,
        ),
    ] {
        let program = resource_program(returning(value, explicit), Vec::new(), false);
        let errors = validation_errors(&program);
        assert!(
            errors.iter().any(|error| {
                error.code == MirValidationCode::ObligationShape
                    && error.path.starts_with("functions[1].body")
                    && error.message.contains("transfer it into Scoped")
            }),
            "{name}: {errors:?}"
        );
    }

    let producer = returning(resource_value(34), false);
    for explicit in [true, false] {
        let call = expr(
            ExprKind::Call {
                target: CallTarget::Direct(FunctionId(2)),
                type_arguments: Vec::new(),
                arguments: Vec::new(),
                witnesses: Vec::new(),
            },
            resource_type(),
        );
        let program = resource_program(returning(call, explicit), vec![producer.clone()], false);
        let expected = if explicit {
            "functions[1].body.statements[0].value"
        } else {
            "functions[1].body.tail"
        };
        assert_main_sink(program, expected);
    }

    let carrier = TypeId(1);
    let carrier_ty = Type::Nominal(carrier, Vec::new());
    let mut record_program = resource_program(
        returning(
            expr(
                ExprKind::Record {
                    ty: carrier,
                    type_arguments: Vec::new(),
                    fields: vec![resource_value(35)],
                    construction: ConstructionMode::Plain,
                },
                carrier_ty.clone(),
            ),
            false,
        ),
        Vec::new(),
        false,
    );
    record_program.types.push(TypeDef {
        id: carrier,
        name: "Carrier".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "resource".to_owned(),
                ty: resource_type(),
                span: span(),
            }],
            invariant: None,
        },
    });
    assert_main_sink(record_program, "functions[1].body.tail");

    let mut variant_program = resource_program(
        returning(
            expr(
                ExprKind::Variant {
                    ty: carrier,
                    type_arguments: Vec::new(),
                    variant: VariantId(0),
                    payload: vec![resource_value(36)],
                },
                carrier_ty.clone(),
            ),
            true,
        ),
        Vec::new(),
        false,
    );
    variant_program.types.push(TypeDef {
        id: carrier,
        name: "Carrier".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Enum {
            variants: vec![VariantDef {
                id: VariantId(0),
                name: "Some".to_owned(),
                payload: vec![resource_type()],
                span: span(),
            }],
        },
    });
    assert_main_sink(variant_program, "functions[1].body.statements[0].value");

    let mut refined_program = resource_program(
        returning(
            expr(
                ExprKind::Refine {
                    ty: carrier,
                    value: Box::new(resource_value(37)),
                    construction: ConstructionMode::Proven,
                },
                carrier_ty,
            ),
            false,
        ),
        Vec::new(),
        false,
    );
    refined_program.types.push(TypeDef {
        id: carrier,
        name: "RefinedResource".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Refined {
            base: resource_type(),
            predicate: Contract {
                code: "always".to_owned(),
                span: span(),
                expression: ContractExpr {
                    kind: ContractExprKind::Constant(Constant::Bool(true)),
                    span: span(),
                },
            },
        },
    });
    assert_main_sink(refined_program, "functions[1].body.tail");
}

#[test]
fn portable_mir_rejects_scoping_a_non_owning_resource_receiver() {
    for receiver in [Receiver::Readonly, Receiver::Mutable] {
        let mutable = receiver == Receiver::Mutable;
        let mut receiver_method = function(
            2,
            vec![local(0, resource_type(), mutable)],
            vec![local(1, resource_type(), true)],
            Type::Unit,
            Block {
                statements: vec![Statement {
                    kind: StatementKind::Scoped {
                        local: LocalId(1),
                        value: copy(0, resource_type()),
                        disposal: ScopedDisposal::StaticConcept {
                            requirement: RequirementId(0),
                            witness: WitnessRef::Concrete(WitnessId(0)),
                            dispatch_type: resource_type(),
                        },
                    },
                    span: span(),
                }],
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        );
        receiver_method.receiver = Some(receiver);
        let receiver_argument = if mutable {
            CallArgument::InOut(Place::local(LocalId(0)))
        } else {
            CallArgument::Value(copy(0, resource_type()))
        };
        let main = function(
            1,
            Vec::new(),
            vec![local(0, resource_type(), true)],
            Type::Unit,
            Block {
                statements: vec![
                    scoped_resource(0, 38),
                    Statement {
                        kind: StatementKind::Evaluate(expr(
                            ExprKind::Call {
                                target: CallTarget::Direct(FunctionId(2)),
                                type_arguments: Vec::new(),
                                arguments: vec![receiver_argument],
                                witnesses: Vec::new(),
                            },
                            Type::Unit,
                        )),
                        span: span(),
                    },
                ],
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        );
        let errors = resource_program(main, vec![receiver_method], false)
            .into_checked()
            .expect_err("a callee cannot acquire ownership by copying its receiver into Scoped");
        assert!(
            errors.iter().any(|error| {
                error.code == MirValidationCode::ObligationShape
                    && error.path == "functions[2].body.statements[0].value"
                    && error.message.contains("non-owning method receiver")
            }),
            "{receiver:?}: {errors:?}"
        );
    }
}

#[test]
fn canonical_file_owning_parameter_may_transfer_into_scoped() {
    let file = TypeId(0);
    let file_ty = Type::Nominal(file, Vec::new());
    let owner = function(
        0,
        vec![local(0, file_ty.clone(), false)],
        vec![local(1, file_ty.clone(), true)],
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Scoped {
                    local: LocalId(1),
                    value: copy(0, file_ty),
                    disposal: ScopedDisposal::StaticConcept {
                        requirement: RequirementId(0),
                        witness: WitnessRef::Concrete(WitnessId(0)),
                        dispatch_type: Type::Nominal(file, Vec::new()),
                    },
                },
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let mut program = resource_program(owner, Vec::new(), false);
    program.types[0].name = "File".to_owned();
    program.prelude.file = Some(file);
    validate_program(&program)
        .expect("an ordinary parameter owns its File and may transfer it into Scoped");
}

#[test]
#[allow(clippy::too_many_lines)]
fn portable_mir_rejects_non_place_resource_receiver_roots() {
    for kind in ["File", "Socket", "Custom"] {
        let mut prelude = PreludeIds::default();
        let (concepts, witnesses) = match kind {
            "File" => {
                prelude.file = Some(TypeId(0));
                (Vec::new(), Vec::new())
            }
            "Socket" => {
                prelude.socket = Some(TypeId(0));
                (Vec::new(), Vec::new())
            }
            "Custom" => {
                prelude.must_scope_concept = Some(ConceptId(0));
                (
                    vec![ConceptDef {
                        id: ConceptId(0),
                        module: "std.resource".to_owned(),
                        name: "MustScope".to_owned(),
                        span: span(),
                        identity: Some(ConceptIdentity::MustScope),
                        dynamic: false,
                        associated_types: Vec::new(),
                        requirements: Vec::new(),
                    }],
                    vec![Witness {
                        id: WitnessId(0),
                        concept: ConceptId(0),
                        concrete: resource_type(),
                        methods: BTreeMap::new(),
                        associated: BTreeMap::new(),
                        type_parameters: 0,
                        prerequisites: Vec::new(),
                    }],
                )
            }
            _ => unreachable!(),
        };
        let make_program = |functions| Program {
            types: vec![raw_handle_type(0, kind)],
            concepts: concepts.clone(),
            functions,
            witnesses: witnesses.clone(),
            prelude,
            ..Program::default()
        };
        let mut receiver_method = function(
            1,
            vec![local(0, resource_type(), false)],
            Vec::new(),
            Type::Unit,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        );
        receiver_method.receiver = Some(Receiver::Readonly);
        let receiver_call = |value| {
            expr(
                ExprKind::Call {
                    target: CallTarget::Inherent(FunctionId(1)),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(value)],
                    witnesses: Vec::new(),
                },
                Type::Unit,
            )
        };
        let make_main = |value| {
            function(
                0,
                Vec::new(),
                Vec::new(),
                Type::Unit,
                Block {
                    statements: vec![Statement {
                        kind: StatementKind::Evaluate(receiver_call(value)),
                        span: span(),
                    }],
                    tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                    span: span(),
                },
            )
        };
        let assert_receiver_error = |program: Program, root: &str| {
            let errors = validation_errors(&program);
            assert!(
                errors.iter().any(|error| {
                    error.code == MirValidationCode::ObligationShape
                        && error.path == "functions[0].body.statements[0].expression.arguments[0]"
                        && error.message.contains("complete active scoped binding")
                }),
                "{kind} {root}: {errors:?}"
            );
        };

        assert_receiver_error(
            make_program(vec![make_main(resource_value(50)), receiver_method.clone()]),
            "fresh",
        );

        let producer = function(
            2,
            Vec::new(),
            Vec::new(),
            resource_type(),
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(resource_value(51))),
                span: span(),
            },
        );
        let call_result = expr(
            ExprKind::Call {
                target: CallTarget::Direct(FunctionId(2)),
                type_arguments: Vec::new(),
                arguments: Vec::new(),
                witnesses: Vec::new(),
            },
            resource_type(),
        );
        assert_receiver_error(
            make_program(vec![
                make_main(call_result),
                receiver_method.clone(),
                producer,
            ]),
            "call",
        );

        let task_ty = Type::Task(Box::new(resource_type()));
        let awaited = expr(
            ExprKind::Await {
                state: 1,
                task: Box::new(move_local(0, task_ty.clone())),
            },
            resource_type(),
        );
        let mut asynchronous = function(
            0,
            vec![local(0, task_ty, false)],
            Vec::new(),
            Type::Unit,
            Block {
                statements: vec![Statement {
                    kind: StatementKind::Evaluate(receiver_call(awaited)),
                    span: span(),
                }],
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        );
        asynchronous.is_async = true;
        asynchronous.suspension_points = loom_mir::analyze_suspension_liveness(&asynchronous.body)
            .into_iter()
            .map(|(state, live_locals)| SuspensionPoint {
                state,
                span: span(),
                live_locals,
            })
            .collect();
        assert_receiver_error(make_program(vec![asynchronous, receiver_method]), "await");
    }
}

#[test]
fn canonical_file_obligation_propagates_through_composite_places_without_markers() {
    let file = TypeId(0);
    let wrapper = TypeId(1);
    let file_ty = Type::Nominal(file, Vec::new());
    let wrapper_ty = Type::Nominal(wrapper, Vec::new());
    let sink = function(
        1,
        vec![local(0, wrapper_ty.clone(), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let main = function(
        0,
        Vec::new(),
        vec![local(0, wrapper_ty.clone(), false)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: expr(
                            ExprKind::Record {
                                ty: wrapper,
                                type_arguments: Vec::new(),
                                fields: vec![expr(
                                    ExprKind::Record {
                                        ty: file,
                                        type_arguments: Vec::new(),
                                        fields: vec![constant(Constant::Int(42), Type::Int)],
                                        construction: ConstructionMode::Plain,
                                    },
                                    file_ty.clone(),
                                )],
                                construction: ConstructionMode::Plain,
                            },
                            wrapper_ty.clone(),
                        ),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Call {
                            target: CallTarget::Direct(FunctionId(1)),
                            type_arguments: Vec::new(),
                            arguments: vec![CallArgument::Value(move_local(0, wrapper_ty.clone()))],
                            witnesses: Vec::new(),
                        },
                        Type::Unit,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let program = Program {
        types: vec![
            raw_handle_type(file.0, "File"),
            TypeDef {
                id: wrapper,
                name: "FileBox".to_owned(),
                span: span(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "file".to_owned(),
                        ty: file_ty,
                        span: span(),
                    }],
                    invariant: None,
                },
            },
        ],
        functions: vec![main, sink],
        exports: BTreeMap::from([("main".to_owned(), FunctionId(0))]),
        prelude: PreludeIds {
            file: Some(file),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    assert!(program.prelude.must_scope_concept.is_none());
    let errors = program
        .into_checked()
        .expect_err("a File obligation cannot escape through a composite ordinary argument");
    assert!(
        errors.iter().any(|error| {
            error.code == MirValidationCode::ObligationShape
                && error.path == "functions[0].body.statements[1].expression.arguments[0]"
        }),
        "{errors:?}"
    );
}

#[test]
fn projected_inout_resource_checks_use_the_final_place_type() {
    let file = TypeId(0);
    let wrapper = TypeId(1);
    let file_ty = Type::Nominal(file, Vec::new());
    let wrapper_ty = Type::Nominal(wrapper, Vec::new());
    let mut touch_int = function(
        1,
        vec![local(0, Type::Int, true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    touch_int.receiver = Some(Receiver::Mutable);
    let mut touch_file = function(
        2,
        vec![local(0, file_ty.clone(), true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    touch_file.receiver = Some(Receiver::Mutable);
    let make_main = |field: u32, target: u32| {
        let mut main = function(
            0,
            vec![local(0, wrapper_ty.clone(), true)],
            Vec::new(),
            Type::Unit,
            Block {
                statements: vec![Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Call {
                            target: CallTarget::Inherent(FunctionId(target)),
                            type_arguments: Vec::new(),
                            arguments: vec![CallArgument::InOut(Place {
                                local: LocalId(0),
                                projection: vec![field],
                            })],
                            witnesses: Vec::new(),
                        },
                        Type::Unit,
                    )),
                    span: span(),
                }],
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        );
        main.receiver = Some(Receiver::Mutable);
        main
    };
    let make_program = |main| Program {
        types: vec![
            raw_handle_type(file.0, "File"),
            TypeDef {
                id: wrapper,
                name: "Wrapper".to_owned(),
                span: span(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![
                        FieldDef {
                            name: "ordinary".to_owned(),
                            ty: Type::Int,
                            span: span(),
                        },
                        FieldDef {
                            name: "resource".to_owned(),
                            ty: file_ty.clone(),
                            span: span(),
                        },
                    ],
                    invariant: None,
                },
            },
        ],
        functions: vec![main, touch_int.clone(), touch_file.clone()],
        prelude: PreludeIds {
            file: Some(file),
            ..PreludeIds::default()
        },
        ..Program::default()
    };

    validate_program(&make_program(make_main(0, 1)))
        .expect("a resource container's ordinary projected field remains an ordinary place");
    let errors = validation_errors(&make_program(make_main(1, 2)));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ObligationShape
            && error.path == "functions[0].body.statements[0].expression.arguments[0]"
            && error.message.contains("complete active scoped binding")
    }));
}

#[test]
fn canonical_file_obligation_budget_exhaustion_fails_closed_without_marker_proofs() {
    const NON_RESOURCE_LEAVES: usize = 4_096;

    for with_marker in [false, true] {
        let file = TypeId(0);
        let file_ty = Type::Nominal(file, Vec::new());
        let mut element_types = vec![Type::Int; NON_RESOURCE_LEAVES];
        element_types.push(file_ty.clone());
        let return_ty = Type::Tuple(element_types);

        let mut elements = (0..NON_RESOURCE_LEAVES)
            .map(|_| constant(Constant::Int(0), Type::Int))
            .collect::<Vec<_>>();
        elements.push(expr(
            ExprKind::Record {
                ty: file,
                type_arguments: Vec::new(),
                fields: vec![constant(Constant::Int(42), Type::Int)],
                construction: ConstructionMode::Plain,
            },
            file_ty,
        ));

        let concepts = with_marker
            .then(|| ConceptDef {
                id: ConceptId(0),
                module: "std.resource".to_owned(),
                name: "MustScope".to_owned(),
                span: span(),
                identity: Some(ConceptIdentity::MustScope),
                dynamic: false,
                associated_types: Vec::new(),
                requirements: Vec::new(),
            })
            .into_iter()
            .collect();
        let program = Program {
            types: vec![raw_handle_type(file.0, "File")],
            concepts,
            functions: vec![function(
                0,
                Vec::new(),
                Vec::new(),
                return_ty.clone(),
                Block {
                    statements: Vec::new(),
                    tail: Some(Box::new(expr(ExprKind::Tuple(elements), return_ty))),
                    span: span(),
                },
            )],
            prelude: PreludeIds {
                file: Some(file),
                must_scope_concept: with_marker.then_some(ConceptId(0)),
                ..PreludeIds::default()
            },
            ..Program::default()
        };

        let errors = validate_program(&program)
            .expect_err("resource analysis budget exhaustion must fail closed");
        assert!(
            errors.iter().any(|error| {
                error.code == MirValidationCode::ObligationShape
                    && error.path == "functions[0].body.tail"
            }),
            "with_marker={with_marker}: {errors:?}"
        );
    }
}

#[test]
fn portable_mir_rejects_no_suspend_resource_across_await() {
    let mut main = function(
        1,
        Vec::new(),
        vec![local(0, resource_type(), true)],
        Type::Unit,
        Block {
            statements: vec![
                scoped_resource(0, 9),
                Statement {
                    kind: StatementKind::Evaluate(sleep_await(1)),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    main.is_async = true;
    main.suspension_points = vec![SuspensionPoint {
        state: 1,
        span: span(),
        live_locals: vec![LocalId(0)],
    }];
    let errors = validation_errors(&resource_program(main, Vec::new(), true));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::SuspensionShape && error.message.contains("NoSuspend")
    }));
}

fn float_program(bits: u64) -> CheckedProgram {
    artifact_program_with_resource_identities(Program {
        functions: vec![function(
            0,
            Vec::new(),
            Vec::new(),
            Type::Float,
            Block {
                statements: vec![
                    Statement {
                        kind: StatementKind::Evaluate(constant(Constant::Float(-0.0), Type::Float)),
                        span: span(),
                    },
                    Statement {
                        kind: StatementKind::Evaluate(constant(
                            Constant::Float(f64::INFINITY),
                            Type::Float,
                        )),
                        span: span(),
                    },
                ],
                tail: Some(Box::new(constant(
                    Constant::Float(f64::from_bits(bits)),
                    Type::Float,
                ))),
                span: span(),
            },
        )],
        exports: BTreeMap::from([("main".to_owned(), FunctionId(0))]),
        ..Program::default()
    })
}

fn forged_proof_program() -> CheckedProgram {
    let rejected = || Contract {
        code: "always.reject".to_owned(),
        span: span(),
        expression: ContractExpr {
            kind: ContractExprKind::Constant(Constant::Bool(false)),
            span: span(),
        },
    };
    let refined = TypeId(0);
    let guarded = TypeId(1);
    artifact_program_with_resource_identities(Program {
        types: vec![
            TypeDef {
                id: refined,
                name: "Positive".to_owned(),
                span: span(),
                type_parameters: 0,
                kind: TypeDefKind::Refined {
                    base: Type::Int,
                    predicate: rejected(),
                },
            },
            TypeDef {
                id: guarded,
                name: "Guarded".to_owned(),
                span: span(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "value".to_owned(),
                        ty: Type::Int,
                        span: span(),
                    }],
                    invariant: Some(rejected()),
                },
            },
        ],
        functions: vec![function(
            0,
            Vec::new(),
            Vec::new(),
            Type::Unit,
            Block {
                statements: vec![
                    Statement {
                        kind: StatementKind::Evaluate(expr(
                            ExprKind::Refine {
                                ty: refined,
                                value: Box::new(constant(Constant::Int(-1), Type::Int)),
                                construction: ConstructionMode::Proven,
                            },
                            Type::Nominal(refined, Vec::new()),
                        )),
                        span: span(),
                    },
                    Statement {
                        kind: StatementKind::Evaluate(expr(
                            ExprKind::Record {
                                ty: guarded,
                                type_arguments: Vec::new(),
                                fields: vec![constant(Constant::Int(-1), Type::Int)],
                                construction: ConstructionMode::Proven,
                            },
                            Type::Nominal(guarded, Vec::new()),
                        )),
                        span: span(),
                    },
                ],
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        exports: BTreeMap::from([("main".to_owned(), FunctionId(0))]),
        ..Program::default()
    })
}

#[test]
fn interpreted_artifact_bytes_are_deterministic_and_round_trip_float_bits() {
    let program = float_program(0x7ff8_0000_0000_0042);
    let first = encode_interpreted_artifact(&program).expect("encode");
    let second = encode_interpreted_artifact(&program).expect("encode again");
    assert_eq!(first, second);

    let decoded = decode_interpreted_artifact(&first).expect("decode");
    let body = &decoded.functions[0].body;
    let StatementKind::Evaluate(Expr {
        kind: ExprKind::Constant(Constant::Float(negative_zero)),
        ..
    }) = &body.statements[0].kind
    else {
        panic!("expected Float");
    };
    assert_eq!(negative_zero.to_bits(), (-0.0_f64).to_bits());
    let ExprKind::Constant(Constant::Float(nan)) = &body.tail.as_ref().expect("tail").kind else {
        panic!("expected Float tail");
    };
    assert_eq!(nan.to_bits(), 0x7ff8_0000_0000_0000);
    assert_eq!(
        encode_interpreted_artifact(&decoded).expect("re-encode"),
        first
    );
}

#[test]
fn artifact_wire_proofs_are_one_way_normalized_to_runtime_rechecks() {
    let program = forged_proof_program();
    assert!(program.requires_serialized_construction_replay());
    assert!(!program.serialized_construction_proofs_were_distrusted());
    let encoded = encode_interpreted_artifact(&program).expect("encode proof fixture");
    let encoded_text = String::from_utf8(encoded.clone()).expect("artifact JSON is UTF-8");
    assert!(!encoded_text.contains("\"construction\":\"proven\""));
    assert_eq!(
        encoded_text.matches("\"construction\":\"recheck\"").count(),
        2
    );

    let decoded = decode_interpreted_artifact(&encoded).expect("decode normalized proof fixture");
    assert!(decoded.serialized_construction_proofs_were_distrusted());
    assert!(decoded.requires_serialized_construction_replay());
    let debug = format!("{decoded:#?}");
    assert_eq!(debug.matches("construction: Recheck").count(), 2, "{debug}");

    let forged = encoded_text.replace(
        "\"construction\":\"recheck\"",
        "\"construction\":\"proven\"",
    );
    let decoded = decode_interpreted_artifact(forged.as_bytes())
        .expect("a forged Proven spelling must be safely normalized");
    let debug = format!("{decoded:#?}");
    assert_eq!(debug.matches("construction: Recheck").count(), 2, "{debug}");
    assert!(!debug.contains("construction: Proven"), "{debug}");
}

#[test]
fn interpreted_executable_artifact_round_trips_and_validates_its_fixed_entry() {
    let program = float_program(1.0_f64.to_bits());
    let bytes =
        encode_interpreted_executable_artifact(&program, "main").expect("encode executable");
    let (decoded, entry) =
        decode_interpreted_executable_artifact(&bytes).expect("decode executable");
    assert!(!decoded.serialized_construction_proofs_were_distrusted());
    assert!(!decoded.requires_serialized_construction_replay());
    assert_eq!(entry, "main");
    assert!(decoded.exports.contains_key(&entry));

    let generic_error = decode_interpreted_artifact(&bytes)
        .expect_err("generic decoder must reject executable artifact bytes");
    assert!(matches!(
        generic_error,
        ArtifactError::UnexpectedEntry { entry } if entry == "main"
    ));

    let generic_bytes = encode_interpreted_artifact(&program).expect("encode generic artifact");
    let executable_error = decode_interpreted_executable_artifact(&generic_bytes)
        .expect_err("executable decoder must reject generic artifact bytes");
    assert!(matches!(executable_error, ArtifactError::MissingEntry));

    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["entry"] = serde_json::json!("missing");
    let error = decode_interpreted_executable_artifact(
        &serde_json::to_vec(&value).expect("tampered executable"),
    )
    .expect_err("unknown artifact entry must fail closed");
    assert!(matches!(
        error,
        ArtifactError::UnknownEntry { entry } if entry == "missing"
    ));
}

#[test]
fn interpreted_artifact_kind_is_rejected_before_program_body_decode() {
    let program = float_program(1.0_f64.to_bits());
    let executable =
        encode_interpreted_executable_artifact(&program, "main").expect("encode executable");
    let mut executable_value: serde_json::Value =
        serde_json::from_slice(&executable).expect("executable JSON");
    executable_value["program"] = serde_json::json!("invalid body must remain unread");
    let generic_error = decode_interpreted_artifact(
        &serde_json::to_vec(&executable_value).expect("encode wrong-kind executable"),
    )
    .expect_err("generic decoder must reject executable kind before its body");
    assert!(matches!(
        generic_error,
        ArtifactError::UnexpectedEntry { entry } if entry == "main"
    ));

    let generic = encode_interpreted_artifact(&program).expect("encode generic artifact");
    let mut generic_value: serde_json::Value =
        serde_json::from_slice(&generic).expect("generic JSON");
    generic_value["program"] = serde_json::json!("invalid body must remain unread");
    let executable_error = decode_interpreted_executable_artifact(
        &serde_json::to_vec(&generic_value).expect("encode wrong-kind generic artifact"),
    )
    .expect_err("executable decoder must reject generic kind before its body");
    assert!(matches!(executable_error, ArtifactError::MissingEntry));
}

#[test]
fn interpreted_artifact_kind_discriminator_is_explicit_and_typed() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits()))
        .expect("encode generic artifact");
    let mut missing: serde_json::Value = serde_json::from_slice(&bytes).expect("artifact JSON");
    missing
        .as_object_mut()
        .expect("artifact object")
        .remove("entry");
    let missing_error =
        decode_interpreted_artifact(&serde_json::to_vec(&missing).expect("missing entry JSON"))
            .expect_err("artifact kind discriminator must be explicit");
    assert!(
        matches!(missing_error, ArtifactError::Malformed(message) if message.contains("missing field `entry`"))
    );

    let mut mistyped: serde_json::Value = serde_json::from_slice(&bytes).expect("artifact JSON");
    mistyped["entry"] = serde_json::json!(7);
    let mistyped_error =
        decode_interpreted_artifact(&serde_json::to_vec(&mistyped).expect("mistyped entry JSON"))
            .expect_err("artifact kind discriminator must be string or null");
    assert!(
        matches!(mistyped_error, ArtifactError::Malformed(message) if message.contains("string or null"))
    );
}

#[test]
fn all_nan_payloads_encode_to_identical_bytes() {
    let left = encode_interpreted_artifact(&float_program(0x7ff0_0000_0000_0001)).expect("left");
    let right = encode_interpreted_artifact(&float_program(0xfff8_0000_0000_1234)).expect("right");
    assert_eq!(left, right);
}

#[test]
fn artifact_rejects_version_mismatch_before_program_decode() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let previous = INTERPRETED_ARTIFACT_VERSION
        .checked_sub(1)
        .expect("artifact version must be positive");
    let next = INTERPRETED_ARTIFACT_VERSION
        .checked_add(1)
        .expect("artifact version must fit u32");
    for found in [previous, next] {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        value["version"] = serde_json::json!(found);
        value["program"] = serde_json::json!("incompatible body");
        let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
            .expect_err("version must fail before body decoding");
        assert!(matches!(
            error,
            ArtifactError::VersionMismatch {
                expected,
                found: actual
            } if expected == INTERPRETED_ARTIFACT_VERSION && actual == u64::from(found)
        ));
    }
}

#[test]
fn artifact_requires_explicit_witness_segmentation() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["program"]["functions"]
        .as_array_mut()
        .and_then(|functions| functions.first_mut())
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|function| function.remove("witness_prefix_count"))
        .expect("encoded function witness segmentation field");
    let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
        .expect_err("function segmentation is required");
    assert!(matches!(
        error,
        ArtifactError::Malformed(message) if message.contains("witness_prefix_count")
    ));
}

#[test]
fn current_artifact_requires_the_exact_current_mir_shape() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let original: serde_json::Value = serde_json::from_slice(&bytes).expect("artifact JSON");

    let mut missing_prelude_field = original.clone();
    missing_prelude_field["program"]["prelude"]
        .as_object_mut()
        .expect("prelude object")
        .remove("bytes")
        .expect("encoded prelude field");
    let missing_error = decode_interpreted_artifact(
        &serde_json::to_vec(&missing_prelude_field).expect("missing-field JSON"),
    )
    .expect_err("matching-version artifacts cannot inherit omitted fields");
    assert!(
        matches!(missing_error, ArtifactError::Malformed(ref message) if message.contains("missing field `bytes`")),
        "{missing_error:?}"
    );

    let mut missing_function_field = original.clone();
    missing_function_field["program"]["functions"][0]
        .as_object_mut()
        .expect("function object")
        .remove("is_async")
        .expect("encoded function field");
    let missing_error = decode_interpreted_artifact(
        &serde_json::to_vec(&missing_function_field).expect("missing-field JSON"),
    )
    .expect_err("matching-version functions cannot inherit omitted fields");
    assert!(
        matches!(missing_error, ArtifactError::Malformed(ref message) if message.contains("missing field `is_async`")),
        "{missing_error:?}"
    );

    let mut unknown_field = original;
    unknown_field["program"]["prelude"]["unexpected"] = serde_json::Value::Null;
    let unknown_error = decode_interpreted_artifact(
        &serde_json::to_vec(&unknown_field).expect("unknown-field JSON"),
    )
    .expect_err("matching-version artifacts cannot carry unknown MIR fields");
    assert!(
        matches!(unknown_error, ArtifactError::Malformed(ref message) if message.contains("unknown field `unexpected`")),
        "{unknown_error:?}"
    );

    let mut unknown_span_field =
        serde_json::from_slice::<serde_json::Value>(&bytes).expect("artifact JSON");
    unknown_span_field["program"]["functions"][0]["span"]["unexpected"] = serde_json::Value::Null;
    let unknown_error = decode_interpreted_artifact(
        &serde_json::to_vec(&unknown_span_field).expect("unknown-span-field JSON"),
    )
    .expect_err("matching-version artifacts cannot carry unknown span fields");
    assert!(
        matches!(unknown_error, ArtifactError::Malformed(ref message) if message.contains("unknown field `unexpected`")),
        "{unknown_error:?}"
    );
}

#[test]
fn artifact_rejects_language_version_mismatch_before_program_decode() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["languageVersion"] = serde_json::json!("0.4");
    value["program"] = serde_json::json!("future incompatible body");
    let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
        .expect_err("language version must fail before body decode");
    assert!(matches!(
        error,
        ArtifactError::LanguageVersionMismatch {
            expected: "0.3",
            found
        } if found == "0.4"
    ));
}

#[test]
fn artifact_rejects_float_table_tampering() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["floatBits"] = serde_json::json!([]);
    let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
        .expect_err("float table mismatch");
    assert!(matches!(error, ArtifactError::FloatTableMismatch { .. }));
}

#[test]
fn artifact_decode_runs_mir_validation() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["program"]["functions"][0]["id"] = serde_json::json!(41);
    let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
        .expect_err("invalid MIR");
    assert!(matches!(error, ArtifactError::InvalidProgram(_)));

    let recursive = TypeId(0);
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["program"]["types"] = serde_json::to_value([TypeDef {
        id: recursive,
        name: "ForgedLoop".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "next".to_owned(),
                ty: Type::Nominal(recursive, Vec::new()),
                span: span(),
            }],
            invariant: None,
        },
    }])
    .expect("recursive type JSON");
    let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
        .expect_err("cached recursive MIR must fail the checked boundary");
    assert!(matches!(
        error,
        ArtifactError::InvalidProgram(errors)
            if errors.contains(MirValidationCode::RecursiveValueType)
    ));
}

#[test]
fn pattern_variant_indices_are_validated() {
    let program = Program {
        types: shape_types(),
        functions: vec![function(
            0,
            vec![local(0, Type::Nominal(TypeId(1), Vec::new()), false)],
            Vec::new(),
            Type::Int,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Match {
                        scrutinee: Box::new(copy(0, Type::Nominal(TypeId(1), Vec::new()))),
                        arms: vec![loom_mir::MatchArm {
                            pattern: Pattern::Variant {
                                ty: TypeId(1),
                                variant: VariantId(9),
                                payload: Vec::new(),
                            },
                            bindings: Vec::new(),
                            value: constant(Constant::Int(0), Type::Int),
                        }],
                    },
                    ty: Type::Int,
                    span: span(),
                })),
                span: span(),
            },
        )],
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::InvalidVariantReference));
}

#[test]
fn call_arguments_validate_inout_place_shape() {
    let mut target_function = function(
        0,
        vec![local(0, Type::Int, true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    target_function.receiver = Some(loom_mir::Receiver::Mutable);
    let calling_function = function(
        1,
        vec![local(0, Type::Int, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Call {
                    target: CallTarget::Inherent(FunctionId(0)),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::InOut(Place::local(LocalId(0)))],
                    witnesses: Vec::new(),
                },
                ty: Type::Unit,
                span: span(),
            })),
            span: span(),
        },
    );
    let program = Program {
        functions: vec![target_function, calling_function],
        ..Program::default()
    };
    assert!(validation_errors(&program).contains(MirValidationCode::ImmutablePlace));
}

#[test]
fn mutable_receiver_cannot_move_the_callers_inout_place() {
    let mut method = function(
        0,
        vec![local(0, Type::Int, true)],
        Vec::new(),
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(expr(
                ExprKind::Move(Place::local(LocalId(0))),
                Type::Int,
            ))),
            span: span(),
        },
    );
    method.receiver = Some(Receiver::Mutable);
    let caller = function(
        1,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(0)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Call {
                            target: CallTarget::Inherent(FunctionId(0)),
                            type_arguments: Vec::new(),
                            arguments: vec![CallArgument::InOut(Place::local(LocalId(0)))],
                            witnesses: Vec::new(),
                        },
                        Type::Int,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![method, caller],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ReceiverShape
            && error.path == "functions[0].body.tail.place"
    }));
}

#[test]
fn mutable_receiver_may_assign_its_whole_value() {
    let mut method = function(
        0,
        vec![local(0, Type::Int, true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Assign {
                    place: Place::local(LocalId(0)),
                    value: constant(Constant::Int(2), Type::Int),
                },
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    method.receiver = Some(Receiver::Mutable);
    let caller = function(
        1,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(0)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Call {
                            target: CallTarget::Inherent(FunctionId(0)),
                            type_arguments: Vec::new(),
                            arguments: vec![CallArgument::InOut(Place::local(LocalId(0)))],
                            witnesses: Vec::new(),
                        },
                        Type::Unit,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![method, caller],
        ..Program::default()
    })
    .expect("a mutable receiver may overwrite its aliased value without moving it out");
}

#[test]
fn for_range_induction_binding_is_immutable_at_the_checked_boundary() {
    let function = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::ForRange {
                    local: LocalId(0),
                    start: Box::new(constant(Constant::Int(0), Type::Int)),
                    end: Box::new(constant(Constant::Int(2), Type::Int)),
                    body: Box::new(Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                        span: span(),
                    }),
                },
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let errors = validation_errors(&Program {
        functions: vec![function],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::ImmutablePlace
            && error.message.contains("induction binding")
    }));
}

#[test]
fn for_range_induction_binding_must_be_uninitialized_at_loop_entry() {
    let function = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, false)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(9), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::ForRange {
                        local: LocalId(0),
                        start: Box::new(constant(Constant::Int(0), Type::Int)),
                        end: Box::new(constant(Constant::Int(1), Type::Int)),
                        body: Box::new(Block {
                            statements: Vec::new(),
                            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                            span: span(),
                        }),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let errors = validation_errors(&Program {
        functions: vec![function],
        ..Program::default()
    });
    assert!(errors.as_slice().iter().any(|error| {
        error.code == MirValidationCode::LocalState
            && error
                .message
                .contains("must be uninitialized at loop entry")
    }));
}

fn range_body_that_moves_outer_local(end: i64) -> Function {
    function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, false), local(1, Type::Int, false)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::ForRange {
                        local: LocalId(1),
                        start: Box::new(constant(Constant::Int(0), Type::Int)),
                        end: Box::new(constant(Constant::Int(end), Type::Int)),
                        body: Box::new(Block {
                            statements: vec![Statement {
                                kind: StatementKind::Evaluate(expr(
                                    ExprKind::Move(Place::local(LocalId(0))),
                                    Type::Int,
                                )),
                                span: span(),
                            }],
                            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                            span: span(),
                        }),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    )
}

#[test]
fn continuing_for_range_backedge_must_preserve_loop_entry_locals() {
    // Checked MIR deliberately does not prove exact trip counts from constant
    // syntax. Zero-, one-, and multi-iteration-looking ranges all need a
    // valid continuing backedge because later lowering is shape-independent.
    for end in [0, 1, 2] {
        let errors = validation_errors(&Program {
            functions: vec![range_body_that_moves_outer_local(end)],
            ..Program::default()
        });
        assert!(errors.as_slice().iter().any(|error| {
            error.code == MirValidationCode::LocalState
                && error.message.contains("continuing ForRange body")
        }));
    }
}

#[test]
fn diverging_for_range_body_preserves_only_the_zero_iteration_state() {
    let function = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, false), local(1, Type::Int, false)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::ForRange {
                        local: LocalId(1),
                        start: Box::new(constant(Constant::Int(0), Type::Int)),
                        end: Box::new(constant(Constant::Int(2), Type::Int)),
                        body: Box::new(Block {
                            statements: vec![Statement {
                                kind: StatementKind::Return(Some(constant(
                                    Constant::Unit,
                                    Type::Unit,
                                ))),
                                span: span(),
                            }],
                            tail: None,
                            span: span(),
                        }),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(copy(0, Type::Int)),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![function],
        ..Program::default()
    })
    .expect("only the zero-iteration path continues after a diverging body");
}

#[test]
fn continuing_for_range_body_may_restore_a_moved_loop_entry_local() {
    let function = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, true), local(1, Type::Int, false)],
        Type::Int,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::ForRange {
                        local: LocalId(1),
                        start: Box::new(constant(Constant::Int(0), Type::Int)),
                        end: Box::new(constant(Constant::Int(2), Type::Int)),
                        body: Box::new(Block {
                            statements: vec![
                                Statement {
                                    kind: StatementKind::Evaluate(expr(
                                        ExprKind::Move(Place::local(LocalId(0))),
                                        Type::Int,
                                    )),
                                    span: span(),
                                },
                                Statement {
                                    kind: StatementKind::Assign {
                                        place: Place::local(LocalId(0)),
                                        value: constant(Constant::Int(2), Type::Int),
                                    },
                                    span: span(),
                                },
                            ],
                            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                            span: span(),
                        }),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(copy(0, Type::Int))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![function],
        ..Program::default()
    })
    .expect("a continuing backedge that restores its outer local is valid");
}

fn cleanup_copying(local: u32) -> Block {
    Block {
        statements: vec![Statement {
            kind: StatementKind::Evaluate(copy(local, Type::Int)),
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    }
}

fn cleanup_assigning(local: u32, value: i64) -> Block {
    Block {
        statements: vec![Statement {
            kind: StatementKind::Assign {
                place: Place::local(LocalId(local)),
                value: constant(Constant::Int(value), Type::Int),
            },
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    }
}

fn mutable_receiver_with_int_argument(id: u32) -> Function {
    let mut target = function(
        id,
        vec![local(0, Type::Int, true), local(1, Type::Int, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    target.receiver = Some(Receiver::Mutable);
    target
}

fn checked_division_by_zero() -> Expr {
    expr(
        ExprKind::Binary(
            BinaryOp::Divide,
            Box::new(constant(Constant::Int(1), Type::Int)),
            Box::new(constant(Constant::Int(0), Type::Int)),
        ),
        Type::Int,
    )
}

fn two_argument_inherent_call(target: u32, second: Expr) -> Expr {
    expr(
        ExprKind::Call {
            target: CallTarget::Inherent(FunctionId(target)),
            type_arguments: Vec::new(),
            arguments: vec![
                CallArgument::InOut(Place::local(LocalId(0))),
                CallArgument::Value(second),
            ],
            witnesses: Vec::new(),
        },
        Type::Unit,
    )
}

fn function_with_flat_assert_cleanups(count: usize) -> Function {
    let cleanup = || Block {
        statements: vec![Statement {
            kind: StatementKind::Assert {
                condition: constant(Constant::Bool(true), Type::Bool),
            },
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    function(
        0,
        Vec::new(),
        Vec::new(),
        Type::Unit,
        Block {
            statements: (0..count)
                .map(|_| Statement {
                    kind: StatementKind::Defer(cleanup()),
                    span: span(),
                })
                .collect(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    )
}

#[test]
fn flat_faulting_cleanup_paths_use_a_linear_abstract_unwind() {
    // The cleanup syntax is flat. Recursive path enumeration used to trip the
    // syntax nesting limit and grow exponentially with the registration count.
    for count in [64, 128] {
        validate_program(&Program {
            functions: vec![function_with_flat_assert_cleanups(count)],
            ..Program::default()
        })
        .unwrap_or_else(|errors| panic!("{count} flat cleanups must validate: {errors}"));
    }
}

#[test]
fn very_large_flat_cleanup_stack_has_iterative_storage_and_destruction() {
    const COUNT: usize = 100_000;
    let function = function(
        0,
        Vec::new(),
        Vec::new(),
        Type::Unit,
        Block {
            statements: (0..COUNT)
                .map(|_| Statement {
                    kind: StatementKind::Defer(Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                        span: span(),
                    }),
                    span: span(),
                })
                .collect(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![function],
        ..Program::default()
    })
    .expect("a flat cleanup stack must not become recursive storage");
}

#[test]
fn sibling_cleanup_heads_join_into_the_same_outer_suffix() {
    let branch = |restored| Block {
        statements: vec![
            Statement {
                kind: StatementKind::Defer(cleanup_assigning(1, restored)),
                span: span(),
            },
            Statement {
                kind: StatementKind::Assert {
                    condition: constant(Constant::Bool(true), Type::Bool),
                },
                span: span(),
            },
        ],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let function = function(
        0,
        vec![local(0, Type::Bool, false)],
        vec![local(1, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(1)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Move(Place::local(LocalId(1))),
                        Type::Int,
                    )),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::If {
                            condition: Box::new(copy(0, Type::Bool)),
                            then_branch: branch(10),
                            else_branch: branch(20),
                        },
                        Type::Unit,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![function],
        ..Program::default()
    })
    .expect("both sibling unwind heads restore the local before their shared outer cleanup");
}

#[test]
fn returning_branch_and_normal_fallthrough_both_reach_outer_cleanup() {
    let function = function(
        0,
        vec![local(0, Type::Bool, false)],
        vec![local(1, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(1)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Move(Place::local(LocalId(1))),
                        Type::Int,
                    )),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::If {
                            condition: Box::new(copy(0, Type::Bool)),
                            then_branch: Block {
                                statements: vec![
                                    Statement {
                                        kind: StatementKind::Assign {
                                            place: Place::local(LocalId(1)),
                                            value: constant(Constant::Int(10), Type::Int),
                                        },
                                        span: span(),
                                    },
                                    Statement {
                                        kind: StatementKind::Return(Some(constant(
                                            Constant::Unit,
                                            Type::Unit,
                                        ))),
                                        span: span(),
                                    },
                                ],
                                tail: None,
                                span: span(),
                            },
                            else_branch: Block {
                                statements: vec![Statement {
                                    kind: StatementKind::Assign {
                                        place: Place::local(LocalId(1)),
                                        value: constant(Constant::Int(20), Type::Int),
                                    },
                                    span: span(),
                                }],
                                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                                span: span(),
                            },
                        },
                        Type::Unit,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![function],
        ..Program::default()
    })
    .expect("return unwind and normal fallthrough must both validate their outer cleanup state");
}

#[test]
fn many_fault_points_share_one_pending_cleanup_suffix() {
    const COUNT: usize = 8_192;
    let mut statements = (0..COUNT)
        .map(|_| Statement {
            kind: StatementKind::Defer(Block {
                statements: Vec::new(),
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            }),
            span: span(),
        })
        .collect::<Vec<_>>();
    statements.extend((0..COUNT).map(|_| Statement {
        kind: StatementKind::Assert {
            condition: constant(Constant::Bool(true), Type::Bool),
        },
        span: span(),
    }));

    validate_program(&Program {
        functions: vec![function(
            0,
            Vec::new(),
            Vec::new(),
            Type::Unit,
            Block {
                statements,
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        ..Program::default()
    })
    .expect("fault points with one active suffix must share one unwind transfer");
}

#[test]
fn interleaved_fault_heads_drain_in_cleanup_arena_order() {
    const COUNT: usize = 8_192;
    let statements = (0..COUNT)
        .flat_map(|_| {
            [
                Statement {
                    kind: StatementKind::Defer(Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                        span: span(),
                    }),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assert {
                        condition: constant(Constant::Bool(true), Type::Bool),
                    },
                    span: span(),
                },
            ]
        })
        .collect();

    validate_program(&Program {
        functions: vec![function(
            0,
            Vec::new(),
            Vec::new(),
            Type::Unit,
            Block {
                statements,
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        ..Program::default()
    })
    .expect("distinct pending heads must each transfer once in descending arena order");
}

#[test]
fn many_locals_and_pending_heads_share_identical_persistent_state() {
    const COUNT: u32 = 8_192;
    let locals = (0..COUNT)
        .map(|id| local(id, Type::Int, false))
        .collect::<Vec<_>>();
    let statements = (0..COUNT)
        .flat_map(|_| {
            [
                Statement {
                    kind: StatementKind::Defer(Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                        span: span(),
                    }),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assert {
                        condition: constant(Constant::Bool(true), Type::Bool),
                    },
                    span: span(),
                },
            ]
        })
        .collect();

    validate_program(&Program {
        functions: vec![function(
            0,
            Vec::new(),
            locals,
            Type::Unit,
            Block {
                statements,
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        ..Program::default()
    })
    .expect("dense declarations must not be cloned into every pending cleanup head");
}

#[test]
fn distinct_slot_updates_join_persistently_across_cleanup_suffixes() {
    const COUNT: u32 = 8_192;
    let locals = (0..COUNT)
        .map(|id| local(id, Type::Int, false))
        .collect::<Vec<_>>();
    let statements = (0..COUNT)
        .flat_map(|id| {
            [
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(id),
                        value: constant(Constant::Int(i64::from(id)), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                        span: span(),
                    }),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assert {
                        condition: constant(Constant::Bool(true), Type::Bool),
                    },
                    span: span(),
                },
            ]
        })
        .collect();

    validate_program(&Program {
        functions: vec![function(
            0,
            Vec::new(),
            locals,
            Type::Unit,
            Block {
                statements,
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        ..Program::default()
    })
    .expect("suffix joins with one new slot per head must remain polynomial and exact");
}

#[test]
fn forbidden_stored_view_carriers_are_sanitized_after_primary_diagnostics() {
    const COUNT: u32 = 4_096;
    let view = view_type(false);
    let locals = (1..=COUNT)
        .map(|id| local(id, view.clone(), false))
        .collect::<Vec<_>>();
    let statements = (1..=COUNT)
        .flat_map(|id| {
            [
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(id),
                        value: expr(
                            ExprKind::ReborrowView {
                                owner: Place::local(LocalId(0)),
                                mutable: false,
                                token: id,
                            },
                            view.clone(),
                        ),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                        span: span(),
                    }),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assert {
                        condition: constant(Constant::Bool(true), Type::Bool),
                    },
                    span: span(),
                },
            ]
        })
        .collect();
    let borrower = function(
        0,
        vec![local(0, view, false)],
        locals,
        Type::Unit,
        Block {
            statements,
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        concepts: vec![empty_dyn_concept()],
        functions: vec![borrower],
        ..Program::default()
    });
    assert!(errors.contains(MirValidationCode::BorrowShape));
}

#[test]
fn forbidden_wide_aggregate_view_carriers_do_not_accumulate_dataflow_loans() {
    const COUNT: u32 = 8_192;
    let view = view_type(false);
    let params = (0..COUNT)
        .map(|id| local(id, view.clone(), false))
        .collect::<Vec<_>>();
    let elements = (0..COUNT)
        .map(|id| {
            expr(
                ExprKind::ReborrowView {
                    owner: Place::local(LocalId(id)),
                    mutable: false,
                    token: id,
                },
                view.clone(),
            )
        })
        .collect::<Vec<_>>();
    let aggregate_ty = Type::Tuple(vec![view; COUNT as usize]);
    let aggregate = expr(ExprKind::Tuple(elements), aggregate_ty);
    let borrower = function(
        0,
        params,
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(aggregate),
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        concepts: vec![empty_dyn_concept()],
        functions: vec![borrower],
        ..Program::default()
    });
    assert!(errors.contains(MirValidationCode::BorrowShape));
}

#[test]
fn wide_sync_call_indexes_readonly_borrows_without_prefix_scans() {
    const COUNT: u32 = 8_192;
    let view = view_type(false);
    let sink = function(
        0,
        (0..COUNT)
            .map(|id| local(id, view.clone(), false))
            .collect(),
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let arguments = (0..COUNT)
        .map(|token| {
            CallArgument::Value(expr(
                ExprKind::ReborrowView {
                    owner: Place::local(LocalId(0)),
                    mutable: false,
                    token,
                },
                view.clone(),
            ))
        })
        .collect();
    let entry = function(
        1,
        vec![local(0, view, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(expr(
                ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(0)),
                    type_arguments: Vec::new(),
                    arguments,
                    witnesses: Vec::new(),
                },
                Type::Unit,
            ))),
            span: span(),
        },
    );

    validate_program(&Program {
        concepts: vec![empty_dyn_concept()],
        functions: vec![sink, entry],
        ..Program::default()
    })
    .expect("wide readonly call arguments must use the mutable-loan index");
}

#[test]
fn distinct_cleanup_fault_states_join_once_per_remaining_suffix() {
    const COUNT: u32 = 64;
    let locals = (0..COUNT)
        .map(|id| local(id, Type::Int, true))
        .collect::<Vec<_>>();
    let mut statements = (0..COUNT)
        .map(|id| Statement {
            kind: StatementKind::Let {
                local: LocalId(id),
                value: constant(Constant::Int(i64::from(id)), Type::Int),
            },
            span: span(),
        })
        .collect::<Vec<_>>();
    statements.extend((0..COUNT).map(|id| Statement {
        kind: StatementKind::Defer(Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Move(Place::local(LocalId(id))),
                        Type::Int,
                    )),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assert {
                        condition: constant(Constant::Bool(true), Type::Bool),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(LocalId(id)),
                        value: constant(Constant::Int(i64::from(id)), Type::Int),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        }),
        span: span(),
    }));

    validate_program(&Program {
        functions: vec![function(
            0,
            Vec::new(),
            locals,
            Type::Unit,
            Block {
                statements,
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )],
        ..Program::default()
    })
    .expect("distinct fault states are joined per cleanup suffix instead of enumerated");
}

#[test]
fn deferred_cleanup_uses_exit_state_not_registration_state() {
    let uninitialized = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(0)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let moved = function(
        1,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Move(Place::local(LocalId(0))),
                        Type::Int,
                    )),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(0)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(LocalId(0)),
                        value: constant(Constant::Int(2), Type::Int),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![uninitialized, moved],
        ..Program::default()
    })
    .expect("defer captures are checked against actual exits, after infallible restoration");
}

#[test]
fn cleanup_fault_state_reaches_every_older_cleanup() {
    let newer = Block {
        statements: vec![
            Statement {
                kind: StatementKind::Evaluate(expr(
                    ExprKind::Move(Place::local(LocalId(1))),
                    Type::Int,
                )),
                span: span(),
            },
            Statement {
                kind: StatementKind::Assert {
                    condition: copy(0, Type::Bool),
                },
                span: span(),
            },
            Statement {
                kind: StatementKind::Assign {
                    place: Place::local(LocalId(1)),
                    value: constant(Constant::Int(2), Type::Int),
                },
                span: span(),
            },
        ],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let function = function(
        0,
        vec![local(0, Type::Bool, false)],
        vec![local(1, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(1)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(newer),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![function],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::LocalState && error.path.contains("statements[1].cleanup")
    }));
}

#[test]
fn fault_entry_drops_argument_reservations_before_cleanup() {
    let caller = function(
        1,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_assigning(0, 2)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(two_argument_inherent_call(
                        0,
                        checked_division_by_zero(),
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let cleanup_caller = function(
        2,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_assigning(0, 2)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(Block {
                        statements: vec![Statement {
                            kind: StatementKind::Evaluate(two_argument_inherent_call(
                                0,
                                checked_division_by_zero(),
                            )),
                            span: span(),
                        }],
                        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                        span: span(),
                    }),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![
            mutable_receiver_with_int_argument(0),
            caller,
            cleanup_caller,
        ],
        ..Program::default()
    })
    .expect("fault and cleanup-fault transitions release all call argument reservations");
}

#[test]
fn normal_nested_cleanup_retains_outer_argument_reservations() {
    let nested_argument = expr(
        ExprKind::Block(Block {
            statements: vec![Statement {
                kind: StatementKind::Defer(cleanup_assigning(0, 2)),
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Int(1), Type::Int))),
            span: span(),
        }),
        Type::Int,
    );
    let caller = function(
        1,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(two_argument_inherent_call(0, nested_argument)),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![mutable_receiver_with_int_argument(0), caller],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::BorrowShape && error.path.contains("cleanup")
    }));
}

fn call_never_function() -> Expr {
    expr(
        ExprKind::Call {
            target: CallTarget::Direct(FunctionId(0)),
            type_arguments: Vec::new(),
            arguments: Vec::new(),
            witnesses: Vec::new(),
        },
        Type::Never,
    )
}

fn never_target_with_int(id: u32) -> Function {
    function(
        id,
        vec![local(0, Type::Int, false)],
        Vec::new(),
        Type::Never,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(expr(
                ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(id)),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(copy(0, Type::Int))],
                    witnesses: Vec::new(),
                },
                Type::Never,
            ))),
            span: span(),
        },
    )
}

fn direct_never_moving(target: u32, local: u32) -> Expr {
    expr(
        ExprKind::Call {
            target: CallTarget::Direct(FunctionId(target)),
            type_arguments: Vec::new(),
            arguments: vec![CallArgument::Value(expr(
                ExprKind::Move(Place::local(LocalId(local))),
                Type::Int,
            ))],
            witnesses: Vec::new(),
        },
        Type::Never,
    )
}

fn restoring_diverging_block() -> Block {
    Block {
        statements: vec![Statement {
            kind: StatementKind::Assign {
                place: Place::local(LocalId(1)),
                value: constant(Constant::Int(2), Type::Int),
            },
            span: span(),
        }],
        tail: Some(Box::new(call_never_function())),
        span: span(),
    }
}

fn function_with_all_diverging_cleanup_control(id: u32, use_match: bool) -> Function {
    let control = if use_match {
        expr(
            ExprKind::Match {
                scrutinee: Box::new(copy(0, Type::Bool)),
                arms: vec![
                    MatchArm {
                        pattern: Pattern::Constant(Constant::Bool(true)),
                        bindings: Vec::new(),
                        value: expr(ExprKind::Block(restoring_diverging_block()), Type::Never),
                    },
                    MatchArm {
                        pattern: Pattern::Constant(Constant::Bool(false)),
                        bindings: Vec::new(),
                        value: expr(ExprKind::Block(restoring_diverging_block()), Type::Never),
                    },
                ],
            },
            Type::Never,
        )
    } else {
        expr(
            ExprKind::If {
                condition: Box::new(copy(0, Type::Bool)),
                then_branch: restoring_diverging_block(),
                else_branch: restoring_diverging_block(),
            },
            Type::Never,
        )
    };
    let newer = Block {
        statements: vec![
            Statement {
                kind: StatementKind::Evaluate(expr(
                    ExprKind::Move(Place::local(LocalId(1))),
                    Type::Int,
                )),
                span: span(),
            },
            Statement {
                kind: StatementKind::Evaluate(control),
                span: span(),
            },
        ],
        tail: None,
        span: span(),
    };
    function(
        id,
        vec![local(0, Type::Bool, false)],
        vec![local(1, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(1)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(newer),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    )
}

#[test]
fn all_diverging_cleanup_control_does_not_recollect_its_pre_branch_state() {
    let diverger = function(
        0,
        Vec::new(),
        Vec::new(),
        Type::Never,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(call_never_function())),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![
            diverger,
            function_with_all_diverging_cleanup_control(1, false),
            function_with_all_diverging_cleanup_control(2, true),
        ],
        ..Program::default()
    })
    .expect("If and Match branches already contribute their exact cleanup exit states");
}

#[test]
fn direct_never_short_circuit_rhs_consumes_its_cleanup_exit() {
    let caller = function(
        1,
        vec![local(0, Type::Bool, false)],
        vec![local(1, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(1)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Binary(
                            BinaryOp::And,
                            Box::new(copy(0, Type::Bool)),
                            Box::new(direct_never_moving(0, 1)),
                        ),
                        Type::Bool,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![never_target_with_int(0), caller],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::LocalState
            && error
                .path
                .contains("functions[1].body.statements[1].cleanup")
    }));
}

#[test]
fn direct_never_match_arm_consumes_its_mixed_exit() {
    let mixed = function(
        1,
        vec![local(0, Type::Bool, false)],
        vec![local(1, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(1)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Match {
                            scrutinee: Box::new(copy(0, Type::Bool)),
                            arms: vec![
                                MatchArm {
                                    pattern: Pattern::Constant(Constant::Bool(true)),
                                    bindings: Vec::new(),
                                    value: direct_never_moving(0, 1),
                                },
                                MatchArm {
                                    pattern: Pattern::Constant(Constant::Bool(false)),
                                    bindings: Vec::new(),
                                    value: constant(Constant::Unit, Type::Unit),
                                },
                            ],
                        },
                        Type::Unit,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![never_target_with_int(0), mixed],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::LocalState
            && error
                .path
                .contains("functions[1].body.statements[1].cleanup")
    }));
}

#[test]
fn direct_never_match_arms_consume_every_diverging_exit() {
    let cleanup = Block {
        statements: vec![Statement {
            kind: StatementKind::Evaluate(expr(
                ExprKind::Tuple(vec![copy(1, Type::Int), copy(2, Type::Int)]),
                Type::Tuple(vec![Type::Int, Type::Int]),
            )),
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let all = function(
        1,
        vec![local(0, Type::Bool, false)],
        vec![local(1, Type::Int, true), local(2, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(2),
                        value: constant(Constant::Int(2), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Match {
                            scrutinee: Box::new(copy(0, Type::Bool)),
                            arms: vec![
                                MatchArm {
                                    pattern: Pattern::Constant(Constant::Bool(true)),
                                    bindings: Vec::new(),
                                    value: direct_never_moving(0, 1),
                                },
                                MatchArm {
                                    pattern: Pattern::Constant(Constant::Bool(false)),
                                    bindings: Vec::new(),
                                    value: direct_never_moving(0, 2),
                                },
                            ],
                        },
                        Type::Never,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![never_target_with_int(0), all],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::LocalState
            && error
                .path
                .contains("functions[1].body.statements[2].cleanup")
            && error.path.contains("elements[0]")
    }));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::LocalState
            && error
                .path
                .contains("functions[1].body.statements[2].cleanup")
            && error.path.contains("elements[1]")
    }));
}

#[test]
fn defer_cleanup_is_checked_against_normal_exit_state() {
    let function = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, false)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(0)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Move(Place::local(LocalId(0))),
                        Type::Int,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![function],
        ..Program::default()
    });
    assert!(errors.as_slice().iter().any(|error| {
        error.code == MirValidationCode::LocalState && error.path.contains("cleanup")
    }));
}

#[test]
fn defer_cleanup_accepts_move_then_restore_before_exit() {
    let function = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(0)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Move(Place::local(LocalId(0))),
                        Type::Int,
                    )),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(LocalId(0)),
                        value: constant(Constant::Int(2), Type::Int),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![function],
        ..Program::default()
    })
    .expect("a nonfallible reassignment restores a deferred cleanup capture");
}

#[test]
fn deferred_cleanups_update_older_cleanups_in_lifo_order() {
    let restoring_cleanup = Block {
        statements: vec![Statement {
            kind: StatementKind::Assign {
                place: Place::local(LocalId(0)),
                value: constant(Constant::Int(2), Type::Int),
            },
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let function = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(0)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(restoring_cleanup),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Move(Place::local(LocalId(0))),
                        Type::Int,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![function],
        ..Program::default()
    })
    .expect("the newer cleanup restores the local before the older cleanup reads it");
}

#[test]
fn defer_cleanup_is_checked_on_each_returning_branch() {
    let returning = |capture: u32| Block {
        statements: vec![
            Statement {
                kind: StatementKind::Evaluate(expr(
                    ExprKind::Move(Place::local(LocalId(capture))),
                    Type::Int,
                )),
                span: span(),
            },
            Statement {
                kind: StatementKind::Return(Some(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        ],
        tail: None,
        span: span(),
    };
    let cleanup = Block {
        statements: vec![Statement {
            kind: StatementKind::Evaluate(expr(
                ExprKind::Tuple(vec![copy(1, Type::Int), copy(2, Type::Int)]),
                Type::Tuple(vec![Type::Int, Type::Int]),
            )),
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let function = function(
        0,
        vec![local(0, Type::Bool, false)],
        vec![local(1, Type::Int, false), local(2, Type::Int, false)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(2),
                        value: constant(Constant::Int(2), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::If {
                            condition: Box::new(copy(0, Type::Bool)),
                            then_branch: returning(1),
                            else_branch: returning(2),
                        },
                        Type::Never,
                    )),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![function],
        ..Program::default()
    });
    for local in [1, 2] {
        assert!(errors.as_slice().iter().any(|error| {
            error.code == MirValidationCode::LocalState
                && error.message.contains(&format!("local #{local}"))
                && error.path.contains("cleanup")
        }));
    }
}

#[test]
fn nested_normal_cleanup_mutation_reaches_the_outer_cleanup() {
    let restoring_cleanup = Block {
        statements: vec![Statement {
            kind: StatementKind::Assign {
                place: Place::local(LocalId(0)),
                value: constant(Constant::Int(2), Type::Int),
            },
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let nested = Block {
        statements: vec![
            Statement {
                kind: StatementKind::Defer(restoring_cleanup),
                span: span(),
            },
            Statement {
                kind: StatementKind::Evaluate(expr(
                    ExprKind::Move(Place::local(LocalId(0))),
                    Type::Int,
                )),
                span: span(),
            },
        ],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let function = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(0)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(ExprKind::Block(nested), Type::Unit)),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    validate_program(&Program {
        functions: vec![function],
        ..Program::default()
    })
    .expect("nested normal cleanup mutations feed the eventual outer cleanup");
}

#[test]
fn fallible_integer_assignment_checks_cleanup_before_the_store() {
    let function = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(i64::MAX), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(0)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(LocalId(0)),
                        value: expr(
                            ExprKind::Binary(
                                BinaryOp::Add,
                                Box::new(expr(ExprKind::Move(Place::local(LocalId(0))), Type::Int)),
                                Box::new(constant(Constant::Int(1), Type::Int)),
                            ),
                            Type::Int,
                        ),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![function],
        ..Program::default()
    });
    assert!(errors.as_slice().iter().any(|error| {
        error.code == MirValidationCode::LocalState && error.path.contains("cleanup")
    }));
}

#[test]
fn fallible_call_checks_cleanup_after_moving_arguments() {
    let target = function(
        0,
        vec![local(0, Type::Int, false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Assert {
                    condition: constant(Constant::Bool(false), Type::Bool),
                },
                span: span(),
            }],
            tail: Some(Box::new(copy(0, Type::Int))),
            span: span(),
        },
    );
    let caller = function(
        1,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup_copying(0)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(LocalId(0)),
                        value: expr(
                            ExprKind::Call {
                                target: CallTarget::Direct(FunctionId(0)),
                                type_arguments: Vec::new(),
                                arguments: vec![CallArgument::Value(expr(
                                    ExprKind::Move(Place::local(LocalId(0))),
                                    Type::Int,
                                ))],
                                witnesses: Vec::new(),
                            },
                            Type::Int,
                        ),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![target, caller],
        ..Program::default()
    });
    assert!(errors.as_slice().iter().any(|error| {
        error.code == MirValidationCode::LocalState && error.path.contains("cleanup")
    }));
}

#[test]
fn failing_assert_checks_cleanup_after_evaluating_its_condition() {
    let cleanup = Block {
        statements: vec![Statement {
            kind: StatementKind::Evaluate(copy(0, Type::Bool)),
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let function = function(
        0,
        Vec::new(),
        vec![local(0, Type::Bool, false)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Bool(false), Type::Bool),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assert {
                        condition: expr(ExprKind::Move(Place::local(LocalId(0))), Type::Bool),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![function],
        ..Program::default()
    });
    assert!(errors.as_slice().iter().any(|error| {
        error.code == MirValidationCode::LocalState && error.path.contains("cleanup")
    }));
}

#[test]
fn await_cancellation_checks_cleanup_after_moving_the_task() {
    let task_type = Type::Task(Box::new(Type::Unit));
    let cleanup = Block {
        statements: vec![Statement {
            kind: StatementKind::Evaluate(copy(0, task_type.clone())),
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let mut asynchronous = function(
        0,
        Vec::new(),
        vec![local(0, task_type.clone(), false)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: expr(
                            ExprKind::Sleep {
                                milliseconds: Box::new(constant(Constant::Int(0), Type::Int)),
                            },
                            task_type.clone(),
                        ),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Await {
                            state: 1,
                            task: Box::new(expr(
                                ExprKind::Move(Place::local(LocalId(0))),
                                task_type,
                            )),
                        },
                        Type::Unit,
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    asynchronous.is_async = true;
    let live_locals = loom_mir::analyze_suspension_liveness(&asynchronous.body)
        .remove(&1)
        .expect("await contributes a suspension state");
    asynchronous.suspension_points = vec![SuspensionPoint {
        state: 1,
        span: span(),
        live_locals,
    }];

    let errors = validation_errors(&Program {
        functions: vec![asynchronous],
        ..Program::default()
    });
    assert!(errors.as_slice().iter().any(|error| {
        error.code == MirValidationCode::LocalState && error.path.contains("cleanup")
    }));
}

#[test]
fn task_join_fault_checks_cleanup_after_moving_its_arguments() {
    let task_type = Type::Task(Box::new(Type::Unit));
    let list_type = Type::List(Box::new(task_type.clone()));
    let cleanup = Block {
        statements: vec![Statement {
            kind: StatementKind::Evaluate(copy(0, list_type.clone())),
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let initial_tasks = expr(
        ExprKind::List(vec![expr(
            ExprKind::Sleep {
                milliseconds: Box::new(constant(Constant::Int(0), Type::Int)),
            },
            task_type,
        )]),
        list_type.clone(),
    );
    let function = function(
        0,
        Vec::new(),
        vec![
            local(0, list_type.clone(), true),
            local(1, list_type.clone(), false),
        ],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: initial_tasks,
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: copy(1, list_type.clone()),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Defer(cleanup),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::TaskJoin {
                            mode: loom_mir::TaskJoinMode::All,
                            arguments: vec![expr(
                                ExprKind::Move(Place::local(LocalId(0))),
                                list_type.clone(),
                            )],
                        },
                        Type::Task(Box::new(Type::List(Box::new(Type::Unit)))),
                    )),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(LocalId(0)),
                        value: copy(1, list_type),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );

    let errors = validation_errors(&Program {
        functions: vec![function],
        ..Program::default()
    });
    assert!(errors.as_slice().iter().any(|error| {
        error.code == MirValidationCode::LocalState && error.path.contains("cleanup")
    }));
}

fn range_with_conditional_move(move_exits: bool) -> Function {
    let moving = Block {
        statements: vec![Statement {
            kind: StatementKind::Evaluate(expr(
                ExprKind::Move(Place::local(LocalId(1))),
                Type::Int,
            )),
            span: span(),
        }],
        tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
        span: span(),
    };
    let exiting = Block {
        statements: vec![Statement {
            kind: StatementKind::Return(Some(constant(Constant::Unit, Type::Unit))),
            span: span(),
        }],
        tail: None,
        span: span(),
    };
    let (then_branch, else_branch) = if move_exits {
        let mut moving_then_exiting = moving;
        moving_then_exiting.statements.push(Statement {
            kind: StatementKind::Return(Some(constant(Constant::Unit, Type::Unit))),
            span: span(),
        });
        moving_then_exiting.tail = None;
        (
            moving_then_exiting,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                span: span(),
            },
        )
    } else {
        (moving, exiting)
    };
    function(
        0,
        vec![local(0, Type::Bool, false)],
        vec![local(1, Type::Int, false), local(2, Type::Int, false)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::ForRange {
                        local: LocalId(2),
                        start: Box::new(constant(Constant::Int(0), Type::Int)),
                        end: Box::new(constant(Constant::Int(2), Type::Int)),
                        body: Box::new(Block {
                            statements: vec![Statement {
                                kind: StatementKind::Evaluate(expr(
                                    ExprKind::If {
                                        condition: Box::new(copy(0, Type::Bool)),
                                        then_branch,
                                        else_branch,
                                    },
                                    Type::Unit,
                                )),
                                span: span(),
                            }],
                            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                            span: span(),
                        }),
                    },
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    )
}

#[test]
fn for_range_backedge_uses_only_continuing_conditional_arms() {
    validate_program(&Program {
        functions: vec![range_with_conditional_move(true)],
        ..Program::default()
    })
    .expect("a move confined to an exiting arm cannot affect the backedge");

    let errors = validation_errors(&Program {
        functions: vec![range_with_conditional_move(false)],
        ..Program::default()
    });
    assert!(errors.as_slice().iter().any(|error| {
        error.code == MirValidationCode::LocalState
            && error.message.contains("continuing ForRange body")
    }));
}

fn short_circuit_with_rhs(
    mutable_value: bool,
    before: Vec<Statement>,
    right: Expr,
    tail: Expr,
) -> Function {
    let mut statements = before;
    statements.push(Statement {
        kind: StatementKind::Evaluate(expr(
            ExprKind::Binary(
                BinaryOp::And,
                Box::new(copy(0, Type::Bool)),
                Box::new(right),
            ),
            Type::Bool,
        )),
        span: span(),
    });
    function(
        0,
        vec![local(0, Type::Bool, false)],
        vec![local(1, Type::Bool, mutable_value)],
        tail.ty.clone(),
        Block {
            statements,
            tail: Some(Box::new(tail)),
            span: span(),
        },
    )
}

#[test]
fn short_circuit_rhs_only_initialization_is_not_unconditional() {
    let right = expr(
        ExprKind::Block(Block {
            statements: vec![Statement {
                kind: StatementKind::Let {
                    local: LocalId(1),
                    value: constant(Constant::Bool(true), Type::Bool),
                },
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Bool(true), Type::Bool))),
            span: span(),
        }),
        Type::Bool,
    );
    let invalid = short_circuit_with_rhs(false, Vec::new(), right, copy(1, Type::Bool));
    let errors = validation_errors(&Program {
        functions: vec![invalid],
        ..Program::default()
    });
    assert!(errors.contains(MirValidationCode::LocalState));
}

#[test]
fn short_circuit_rhs_assignment_preserves_an_available_local() {
    let initialize = Statement {
        kind: StatementKind::Let {
            local: LocalId(1),
            value: constant(Constant::Bool(false), Type::Bool),
        },
        span: span(),
    };
    let right = expr(
        ExprKind::Block(Block {
            statements: vec![Statement {
                kind: StatementKind::Assign {
                    place: Place::local(LocalId(1)),
                    value: constant(Constant::Bool(true), Type::Bool),
                },
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Bool(true), Type::Bool))),
            span: span(),
        }),
        Type::Bool,
    );
    let valid = short_circuit_with_rhs(true, vec![initialize], right, copy(1, Type::Bool));

    validate_program(&Program {
        functions: vec![valid],
        ..Program::default()
    })
    .expect("RHS assignment leaves an already-available local available on both paths");
}

#[test]
fn short_circuit_rhs_move_is_maybe_unavailable_after_the_join() {
    let initialize = Statement {
        kind: StatementKind::Let {
            local: LocalId(1),
            value: constant(Constant::Bool(true), Type::Bool),
        },
        span: span(),
    };
    let invalid = short_circuit_with_rhs(
        false,
        vec![initialize],
        expr(ExprKind::Move(Place::local(LocalId(1))), Type::Bool),
        copy(1, Type::Bool),
    );
    let errors = validation_errors(&Program {
        functions: vec![invalid],
        ..Program::default()
    });
    assert!(errors.contains(MirValidationCode::LocalState));
}

#[test]
fn diverging_short_circuit_rhs_leaves_the_short_path_available() {
    let initialize = Statement {
        kind: StatementKind::Let {
            local: LocalId(1),
            value: constant(Constant::Bool(true), Type::Bool),
        },
        span: span(),
    };
    let divergent_right = expr(
        ExprKind::Block(Block {
            statements: vec![Statement {
                kind: StatementKind::Return(Some(constant(Constant::Bool(false), Type::Bool))),
                span: span(),
            }],
            tail: None,
            span: span(),
        }),
        Type::Never,
    );
    let valid = short_circuit_with_rhs(
        false,
        vec![initialize],
        divergent_right,
        copy(1, Type::Bool),
    );

    validate_program(&Program {
        functions: vec![valid],
        ..Program::default()
    })
    .expect("a diverging RHS cannot consume the short-circuit continuation state");
}

#[test]
#[allow(clippy::too_many_lines)]
fn inout_reservation_allows_reads_but_rejects_aliasing_later_arguments() {
    let view = view_type(true);
    let mut value_target = function(
        0,
        vec![local(0, Type::Int, true), local(1, Type::Int, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    value_target.receiver = Some(Receiver::Mutable);
    let mut unit_target = function(
        1,
        vec![local(0, Type::Int, true), local(1, Type::Unit, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    unit_target.receiver = Some(Receiver::Mutable);
    let mut view_target = function(
        2,
        vec![local(0, Type::Int, true), local(1, view, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    view_target.receiver = Some(Receiver::Mutable);
    let call = |target, owner, value| {
        expr(
            ExprKind::Call {
                target: CallTarget::Inherent(FunctionId(target)),
                type_arguments: Vec::new(),
                arguments: vec![
                    CallArgument::InOut(Place::local(LocalId(owner))),
                    CallArgument::Value(value),
                ],
                witnesses: Vec::new(),
            },
            Type::Unit,
        )
    };
    let caller = function(
        3,
        vec![
            local(0, Type::Int, true),
            local(1, Type::Int, true),
            local(2, Type::Int, true),
            local(3, Type::Int, true),
        ],
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(call(0, 0, copy(0, Type::Int))),
                    span: span(),
                },
                Statement {
                    // Reviewer counterexample: arg0 = InOut(c),
                    // arg1 = Value(Move(c)).
                    kind: StatementKind::Evaluate(call(
                        0,
                        1,
                        expr(ExprKind::Move(Place::local(LocalId(1))), Type::Int),
                    )),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(call(
                        1,
                        2,
                        expr(
                            ExprKind::Block(Block {
                                statements: vec![Statement {
                                    kind: StatementKind::Assign {
                                        place: Place::local(LocalId(2)),
                                        value: constant(Constant::Int(7), Type::Int),
                                    },
                                    span: span(),
                                }],
                                tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
                                span: span(),
                            }),
                            Type::Unit,
                        ),
                    )),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(call(
                        2,
                        3,
                        borrowed_view(Place::local(LocalId(3)), 0, 1, true),
                    )),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let errors = validation_errors(&Program {
        concepts: vec![empty_dyn_concept()],
        functions: vec![value_target, unit_target, view_target, caller],
        witnesses: vec![empty_witness(0, Type::Int)],
        ..Program::default()
    });
    assert!(
        !errors.iter().any(|error| {
            error.code == MirValidationCode::BorrowShape
                && error.path.starts_with("functions[3].body.statements[0].")
        }),
        "{errors:#?}"
    );
    for index in 1..4 {
        assert!(errors.iter().any(|error| {
            error.code == MirValidationCode::BorrowShape
                && error
                    .path
                    .starts_with(&format!("functions[3].body.statements[{index}]."))
        }));
    }
}

#[test]
fn inout_access_ends_after_the_call_returns() {
    let mut target = function(
        0,
        vec![local(0, Type::Int, true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    target.receiver = Some(Receiver::Mutable);
    let call = || {
        expr(
            ExprKind::Call {
                target: CallTarget::Inherent(FunctionId(0)),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::InOut(Place::local(LocalId(0)))],
                witnesses: Vec::new(),
            },
            Type::Unit,
        )
    };
    let caller = function(
        1,
        vec![local(0, Type::Int, true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(call()),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(call()),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    validate_program(&Program {
        functions: vec![target, caller],
        ..Program::default()
    })
    .expect("an inout loan is released when its call returns");
}

#[test]
#[allow(clippy::too_many_lines)]
fn projected_inout_allows_sibling_nested_mutation_but_rejects_parent_reset() {
    let pair = Type::Nominal(TypeId(0), Vec::new());
    let pair_def = TypeDef {
        id: TypeId(0),
        name: "Pair".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![
                FieldDef {
                    name: "left".to_owned(),
                    ty: Type::Int,
                    span: span(),
                },
                FieldDef {
                    name: "right".to_owned(),
                    ty: Type::Int,
                    span: span(),
                },
            ],
            invariant: None,
        },
    };
    let mut outer = function(
        0,
        vec![local(0, Type::Int, true), local(1, Type::Unit, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    outer.receiver = Some(Receiver::Mutable);
    let mut mutate_field = function(
        1,
        vec![local(0, Type::Int, true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    mutate_field.receiver = Some(Receiver::Mutable);
    let mut reset_pair = function(
        2,
        vec![local(0, pair.clone(), true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    reset_pair.receiver = Some(Receiver::Mutable);
    let field = |index| Place {
        local: LocalId(0),
        projection: vec![index],
    };
    let nested_call = |target, place| {
        expr(
            ExprKind::Call {
                target: CallTarget::Inherent(FunctionId(target)),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::InOut(place)],
                witnesses: Vec::new(),
            },
            Type::Unit,
        )
    };
    let outer_call = |nested| {
        expr(
            ExprKind::Call {
                target: CallTarget::Inherent(FunctionId(0)),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::InOut(field(0)), CallArgument::Value(nested)],
                witnesses: Vec::new(),
            },
            Type::Unit,
        )
    };
    let sibling = function(
        3,
        vec![local(0, pair.clone(), true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(outer_call(nested_call(1, field(1))))),
            span: span(),
        },
    );
    validate_program(&Program {
        types: vec![pair_def.clone()],
        functions: vec![
            outer.clone(),
            mutate_field.clone(),
            reset_pair.clone(),
            sibling,
        ],
        ..Program::default()
    })
    .expect("a nested mutable call may exclusively access a sibling projection");

    let exact_alias = function(
        3,
        vec![local(0, pair.clone(), true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(outer_call(nested_call(1, field(0))))),
            span: span(),
        },
    );
    let exact_alias_errors = validation_errors(&Program {
        types: vec![pair_def.clone()],
        functions: vec![
            outer.clone(),
            mutate_field.clone(),
            reset_pair.clone(),
            exact_alias,
        ],
        ..Program::default()
    });
    assert!(exact_alias_errors.iter().any(|error| {
        error.code == MirValidationCode::BorrowShape
            && error.path.contains("arguments[1]")
            && error.path.contains("arguments[0]")
    }));

    let parent_reset = function(
        3,
        vec![local(0, pair, true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(outer_call(nested_call(
                2,
                Place::local(LocalId(0)),
            )))),
            span: span(),
        },
    );
    let errors = validation_errors(&Program {
        types: vec![pair_def],
        functions: vec![outer, mutate_field, reset_pair, parent_reset],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::BorrowShape
            && error.path.contains("arguments[1]")
            && error.path.contains("arguments[0]")
    }));
}

#[allow(clippy::too_many_lines)]
fn conditional_concept_program() -> Program {
    let boxed = Type::Nominal(TypeId(0), vec![Type::Parameter(0)]);
    let boxed_int = Type::Nominal(TypeId(0), vec![Type::Int]);
    let requirement = RequirementDef {
        id: RequirementId(0),
        concept: ConceptId(0),
        name: "equal".to_owned(),
        span: span(),
        receiver: Some(Receiver::Readonly),
        method_type_parameters: 0,
        params: vec![RequirementType::SelfType, RequirementType::SelfType],
        return_ty: RequirementType::Bool,
        witness_params: Vec::new(),
    };
    let concept = ConceptDef {
        id: ConceptId(0),
        module: "test".to_owned(),
        name: "Equatable".to_owned(),
        span: span(),
        identity: None,
        dynamic: false,
        associated_types: Vec::new(),
        requirements: vec![RequirementId(0)],
    };

    let mut int_equal = function(
        0,
        vec![local(0, Type::Int, false), local(1, Type::Int, false)],
        Vec::new(),
        Type::Bool,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Bool(true), Type::Bool))),
            span: span(),
        },
    );
    int_equal.receiver = Some(Receiver::Readonly);

    let prerequisite = WitnessParam {
        target: Type::Parameter(0),
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
        span: span(),
    };
    let mut boxed_equal = function(
        1,
        vec![
            local(0, boxed.clone(), false),
            local(1, boxed.clone(), false),
        ],
        Vec::new(),
        Type::Bool,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Bool(true), Type::Bool))),
            span: span(),
        },
    );
    boxed_equal.type_parameters = 1;
    boxed_equal.receiver = Some(Receiver::Readonly);
    boxed_equal.witness_params.push(prerequisite.clone());
    boxed_equal.witness_prefix_count = 1;

    let caller = function(
        2,
        vec![
            local(0, boxed_int.clone(), false),
            local(1, boxed_int.clone(), false),
        ],
        Vec::new(),
        Type::Bool,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Call {
                    target: CallTarget::StaticConcept {
                        requirement: RequirementId(0),
                        witness: WitnessRef::Apply {
                            witness: WitnessId(1),
                            arguments: vec![WitnessRef::Concrete(WitnessId(0))],
                        },
                        dispatch_type: boxed_int.clone(),
                    },
                    type_arguments: Vec::new(),
                    arguments: vec![
                        CallArgument::Value(copy(0, boxed_int.clone())),
                        CallArgument::Value(copy(1, boxed_int.clone())),
                    ],
                    witnesses: Vec::new(),
                },
                ty: Type::Bool,
                span: span(),
            })),
            span: span(),
        },
    );

    Program {
        types: vec![TypeDef {
            id: TypeId(0),
            name: "Boxed".to_owned(),
            span: span(),
            type_parameters: 1,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "value".to_owned(),
                    ty: Type::Parameter(0),
                    span: span(),
                }],
                invariant: None,
            },
        }],
        concepts: vec![concept],
        requirements: vec![requirement],
        functions: vec![int_equal, boxed_equal, caller],
        witnesses: vec![
            Witness {
                id: WitnessId(0),
                concept: ConceptId(0),
                concrete: Type::Int,
                methods: BTreeMap::from([(RequirementId(0), FunctionId(0))]),
                associated: BTreeMap::new(),
                type_parameters: 0,
                prerequisites: Vec::new(),
            },
            Witness {
                id: WitnessId(1),
                concept: ConceptId(0),
                concrete: boxed,
                methods: BTreeMap::from([(RequirementId(0), FunctionId(1))]),
                associated: BTreeMap::new(),
                type_parameters: 1,
                prerequisites: vec![prerequisite],
            },
        ],
        ..Program::default()
    }
}

#[test]
fn conditional_witness_apply_is_checked_as_a_recursive_proof_tree() {
    let program = conditional_concept_program();
    validate_program(&program).expect("valid conditional conformance proof");

    let checked = artifact_program_with_resource_identities(program.clone());
    let bytes = encode_interpreted_artifact(&checked).expect("encode concept metadata");
    let decoded = decode_interpreted_artifact(&bytes).expect("decode concept metadata");
    assert_eq!(decoded.concepts.len(), 4);
    assert_eq!(decoded.requirements.len(), 2);

    let mut wrong_arity = program.clone();
    let ExprKind::Call {
        target: CallTarget::StaticConcept { witness, .. },
        ..
    } = &mut wrong_arity.functions[2]
        .body
        .tail
        .as_mut()
        .expect("call")
        .kind
    else {
        panic!("expected static call");
    };
    *witness = WitnessRef::Apply {
        witness: WitnessId(1),
        arguments: Vec::new(),
    };
    assert!(validation_errors(&wrong_arity).contains(MirValidationCode::WitnessArity));
}

#[test]
fn witness_method_proofs_are_partitioned_into_conformance_and_requirement_segments() {
    let program = conditional_concept_program();
    validate_program(&program).expect("valid partitioned witness method proofs");
    assert_eq!(program.functions[0].witness_prefix_count, 0);
    assert_eq!(program.functions[1].witness_prefix_count, 1);

    let mut wrong_prefix = program.clone();
    wrong_prefix.functions[1].witness_prefix_count = 0;
    let errors = validation_errors(&wrong_prefix);
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::WitnessArity && error.path.contains("witness_prefix_count")
    }));

    let mut oversized_prefix = program.clone();
    oversized_prefix.functions[1].witness_prefix_count = 2;
    let errors = validation_errors(&oversized_prefix);
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::WitnessArity
            && error.message.contains("exceeds the function")
    }));

    let mut non_witness_prefix = program;
    non_witness_prefix.functions[2].witness_prefix_count = 1;
    non_witness_prefix.functions[2]
        .witness_params
        .push(WitnessParam {
            target: Type::Int,
            concept: ConceptId(0),
            bindings: BTreeMap::new(),
            span: span(),
        });
    let errors = validation_errors(&non_witness_prefix);
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::WitnessShape
            && error.message.contains("only witness methods")
    }));
}

#[test]
fn concept_metadata_and_witness_method_tables_fail_closed() {
    let mut program = conditional_concept_program();
    program.concepts[0].id = ConceptId(7);
    program.witnesses[0].methods.clear();
    program.requirements[0].return_ty = RequirementType::Text;
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::IndexMismatch));
    assert!(errors.contains(MirValidationCode::WitnessShape));
}

fn dynamic_source_metadata() -> (ConceptDef, RequirementDef) {
    (
        ConceptDef {
            id: ConceptId(0),
            module: "test".to_owned(),
            name: "Source".to_owned(),
            span: span(),
            identity: None,
            dynamic: true,
            associated_types: vec![AssociatedTypeDef {
                name: "Item".to_owned(),
                span: span(),
            }],
            requirements: vec![RequirementId(0)],
        },
        RequirementDef {
            id: RequirementId(0),
            concept: ConceptId(0),
            name: "next".to_owned(),
            span: span(),
            receiver: Some(Receiver::Mutable),
            method_type_parameters: 0,
            params: vec![RequirementType::SelfType],
            return_ty: RequirementType::Associated("Item".to_owned()),
            witness_params: Vec::new(),
        },
    )
}

#[test]
fn dynamic_calls_use_requirement_metadata_for_signature_and_result() {
    let (concept, requirement) = dynamic_source_metadata();
    let view_ty = Type::View {
        mutable: true,
        concept: ConceptId(0),
        bindings: BTreeMap::from([("Item".to_owned(), Type::Int)]),
    };
    let dynamic_call = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Call {
            target: CallTarget::Dynamic {
                requirement: RequirementId(0),
            },
            type_arguments: Vec::new(),
            arguments: vec![CallArgument::InOut(Place::local(LocalId(0)))],
            witnesses: Vec::new(),
        },
        ty: Type::Int,
        span: span(),
    };
    let mut consume = function(
        0,
        vec![local(0, view_ty, true)],
        Vec::new(),
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(dynamic_call)),
            span: span(),
        },
    );
    consume.type_parameters = 1;
    consume.witness_params.push(WitnessParam {
        target: Type::Parameter(0),
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
        span: span(),
    });
    let projected = Type::AssociatedProjection {
        witness: 0,
        associated: "Item".to_owned(),
    };
    consume.locals.push(local(1, projected.clone(), false));

    let program = Program {
        concepts: vec![concept],
        requirements: vec![requirement],
        functions: vec![consume],
        ..Program::default()
    };
    validate_program(&program).expect("valid dynamic requirement call and executable projection");

    let mut bad = program;
    bad.functions[0].return_ty = Type::AssociatedProjection {
        witness: 0,
        associated: "Missing".to_owned(),
    };
    assert!(validation_errors(&bad).contains(MirValidationCode::TypeMismatch));
}

#[test]
#[allow(clippy::too_many_lines)]
fn unrefine_contract_bindings_and_diverging_branches_are_explicit() {
    let price_predicate = Contract {
        code: "price".to_owned(),
        span: span(),
        expression: ContractExpr {
            kind: ContractExprKind::Constant(Constant::Bool(true)),
            span: span(),
        },
    };
    let types = vec![
        TypeDef {
            id: TypeId(0),
            name: "Price".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Refined {
                base: Type::Float,
                predicate: price_predicate,
            },
        },
        TypeDef {
            id: TypeId(1),
            name: "Outcome".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Enum {
                variants: vec![VariantDef {
                    id: VariantId(0),
                    name: "Ok".to_owned(),
                    payload: vec![Type::Int],
                    span: span(),
                }],
            },
        },
    ];

    let read_price = function(
        0,
        vec![local(0, Type::Nominal(TypeId(0), Vec::new()), false)],
        Vec::new(),
        Type::Float,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Unrefine(Box::new(copy(0, Type::Nominal(TypeId(0), Vec::new())))),
                ty: Type::Float,
                span: span(),
            })),
            span: span(),
        },
    );

    let mut checked = function(
        1,
        vec![local(0, Type::Nominal(TypeId(1), Vec::new()), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    checked.call_plan.requires.push(Contract {
        code: "payload_positive".to_owned(),
        span: span(),
        expression: ContractExpr {
            kind: ContractExprKind::Match {
                scrutinee: Box::new(ContractExpr {
                    kind: ContractExprKind::Value(ContractValue::Argument(0)),
                    span: span(),
                }),
                arms: vec![ContractArm {
                    pattern: Pattern::Variant {
                        ty: TypeId(1),
                        variant: VariantId(0),
                        payload: vec![Pattern::Binding],
                    },
                    bindings: vec![Type::Int],
                    value: ContractExpr {
                        kind: ContractExprKind::Binary(
                            loom_mir::BinaryOp::GreaterEqual,
                            Box::new(ContractExpr {
                                kind: ContractExprKind::Binding(0),
                                span: span(),
                            }),
                            Box::new(ContractExpr {
                                kind: ContractExprKind::Constant(Constant::Int(0)),
                                span: span(),
                            }),
                        ),
                        span: span(),
                    },
                }],
            },
            span: span(),
        },
    });

    let diverging_if = function(
        2,
        vec![local(0, Type::Bool, false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::If {
                    condition: Box::new(copy(0, Type::Bool)),
                    then_branch: Block {
                        statements: vec![Statement {
                            kind: StatementKind::Return(Some(constant(
                                Constant::Int(1),
                                Type::Int,
                            ))),
                            span: span(),
                        }],
                        tail: None,
                        span: span(),
                    },
                    else_branch: Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(constant(Constant::Int(2), Type::Int))),
                        span: span(),
                    },
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
    );

    let program = Program {
        types,
        functions: vec![read_price, checked, diverging_if],
        ..Program::default()
    };
    let program = artifact_program_with_resource_identities(program);
    let bytes = encode_interpreted_artifact(&program).expect("encode current control-flow nodes");
    decode_interpreted_artifact(&bytes).expect("round trip current control-flow nodes");
}

#[test]
#[allow(clippy::too_many_lines)]
fn method_generics_have_explicit_type_arguments_and_method_proofs() {
    let concepts = vec![
        ConceptDef {
            id: ConceptId(0),
            module: "test".to_owned(),
            name: "Equal".to_owned(),
            span: span(),
            identity: None,
            dynamic: false,
            associated_types: Vec::new(),
            requirements: vec![RequirementId(0)],
        },
        ConceptDef {
            id: ConceptId(1),
            module: "test".to_owned(),
            name: "Echo".to_owned(),
            span: span(),
            identity: None,
            dynamic: false,
            associated_types: Vec::new(),
            requirements: vec![RequirementId(1)],
        },
    ];
    let requirements = vec![
        RequirementDef {
            id: RequirementId(0),
            concept: ConceptId(0),
            name: "equal".to_owned(),
            span: span(),
            receiver: Some(Receiver::Readonly),
            method_type_parameters: 0,
            params: vec![RequirementType::SelfType, RequirementType::SelfType],
            return_ty: RequirementType::Bool,
            witness_params: Vec::new(),
        },
        RequirementDef {
            id: RequirementId(1),
            concept: ConceptId(1),
            name: "echo".to_owned(),
            span: span(),
            receiver: Some(Receiver::Readonly),
            method_type_parameters: 1,
            params: vec![
                RequirementType::SelfType,
                RequirementType::MethodParameter(0),
            ],
            return_ty: RequirementType::MethodParameter(0),
            witness_params: vec![loom_mir::RequirementWitnessParam {
                target: RequirementType::MethodParameter(0),
                concept: ConceptId(0),
                bindings: BTreeMap::new(),
                span: span(),
            }],
        },
    ];

    let mut equal = function(
        0,
        vec![local(0, Type::Int, false), local(1, Type::Int, false)],
        Vec::new(),
        Type::Bool,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Bool(true), Type::Bool))),
            span: span(),
        },
    );
    equal.receiver = Some(Receiver::Readonly);

    let mut echo = function(
        1,
        vec![
            local(0, Type::Int, false),
            local(1, Type::Parameter(0), false),
        ],
        Vec::new(),
        Type::Parameter(0),
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(copy(1, Type::Parameter(0)))),
            span: span(),
        },
    );
    echo.receiver = Some(Receiver::Readonly);
    echo.type_parameters = 1;
    echo.witness_params.push(WitnessParam {
        target: Type::Parameter(0),
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
        span: span(),
    });

    let call = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Call {
            target: CallTarget::StaticConcept {
                requirement: RequirementId(1),
                witness: WitnessRef::Concrete(WitnessId(1)),
                dispatch_type: Type::Int,
            },
            type_arguments: vec![Type::Int],
            arguments: vec![
                CallArgument::Value(copy(0, Type::Int)),
                CallArgument::Value(copy(1, Type::Int)),
            ],
            witnesses: vec![WitnessRef::Concrete(WitnessId(0))],
        },
        ty: Type::Int,
        span: span(),
    };
    let caller = function(
        2,
        vec![local(0, Type::Int, false), local(1, Type::Int, false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(call)),
            span: span(),
        },
    );
    let program = Program {
        concepts,
        requirements,
        functions: vec![equal, echo, caller],
        witnesses: vec![
            Witness {
                id: WitnessId(0),
                concept: ConceptId(0),
                concrete: Type::Int,
                methods: BTreeMap::from([(RequirementId(0), FunctionId(0))]),
                associated: BTreeMap::new(),
                type_parameters: 0,
                prerequisites: Vec::new(),
            },
            Witness {
                id: WitnessId(1),
                concept: ConceptId(1),
                concrete: Type::Int,
                methods: BTreeMap::from([(RequirementId(1), FunctionId(1))]),
                associated: BTreeMap::new(),
                type_parameters: 0,
                prerequisites: Vec::new(),
            },
        ],
        ..Program::default()
    };
    validate_program(&program).expect("method type argument and proof instantiate together");

    let mut missing = program;
    let ExprKind::Call {
        target: CallTarget::StaticConcept { .. },
        type_arguments,
        witnesses,
        ..
    } = &mut missing.functions[2].body.tail.as_mut().expect("call").kind
    else {
        panic!("expected static call");
    };
    type_arguments.clear();
    witnesses.clear();
    let errors = validation_errors(&missing);
    assert!(errors.contains(MirValidationCode::CallArity));
    assert!(errors.contains(MirValidationCode::WitnessArity));
}

#[test]
fn direct_calls_instantiate_associated_projection_from_resolved_proof() {
    let projection = Type::AssociatedProjection {
        witness: 0,
        associated: "Item".to_owned(),
    };
    let concept = ConceptDef {
        id: ConceptId(0),
        module: "test".to_owned(),
        name: "Source".to_owned(),
        span: span(),
        identity: None,
        dynamic: false,
        associated_types: vec![AssociatedTypeDef {
            name: "Item".to_owned(),
            span: span(),
        }],
        requirements: Vec::new(),
    };
    let mut read = function(
        0,
        vec![
            local(0, Type::Parameter(0), false),
            local(1, projection.clone(), false),
        ],
        Vec::new(),
        projection.clone(),
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(copy(1, projection))),
            span: span(),
        },
    );
    read.type_parameters = 1;
    read.witness_params.push(WitnessParam {
        target: Type::Parameter(0),
        concept: ConceptId(0),
        bindings: BTreeMap::from([("Item".to_owned(), Type::Int)]),
        span: span(),
    });
    let caller = function(
        1,
        vec![local(0, Type::Text, false), local(1, Type::Int, false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(0)),
                    type_arguments: vec![Type::Text],
                    arguments: vec![
                        CallArgument::Value(copy(0, Type::Text)),
                        CallArgument::Value(copy(1, Type::Int)),
                    ],
                    witnesses: vec![WitnessRef::Concrete(WitnessId(0))],
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
    );
    let program = Program {
        concepts: vec![concept],
        functions: vec![read, caller],
        witnesses: vec![Witness {
            id: WitnessId(0),
            concept: ConceptId(0),
            concrete: Type::Text,
            methods: BTreeMap::new(),
            associated: BTreeMap::from([("Item".to_owned(), Type::Int)]),
            type_parameters: 0,
            prerequisites: Vec::new(),
        }],
        ..Program::default()
    };
    validate_program(&program).expect("call-site proof resolves T.Item to Int");
}

#[test]
#[allow(clippy::too_many_lines)]
fn parameter_witness_associated_projections_remain_symbolic() {
    let projection = Type::AssociatedProjection {
        witness: 0,
        associated: "Item".to_owned(),
    };
    let source = ConceptDef {
        id: ConceptId(0),
        module: "test".to_owned(),
        name: "Source".to_owned(),
        span: span(),
        identity: None,
        dynamic: false,
        associated_types: vec![AssociatedTypeDef {
            name: "Item".to_owned(),
            span: span(),
        }],
        requirements: vec![RequirementId(0)],
    };
    let convert = ConceptDef {
        id: ConceptId(1),
        module: "test".to_owned(),
        name: "Convert".to_owned(),
        span: span(),
        identity: None,
        dynamic: false,
        associated_types: Vec::new(),
        requirements: vec![RequirementId(1)],
    };
    let first = RequirementDef {
        id: RequirementId(0),
        concept: ConceptId(0),
        name: "first".to_owned(),
        span: span(),
        receiver: Some(Receiver::Readonly),
        method_type_parameters: 0,
        params: vec![RequirementType::SelfType],
        return_ty: RequirementType::Associated("Item".to_owned()),
        witness_params: Vec::new(),
    };
    let get = RequirementDef {
        id: RequirementId(1),
        concept: ConceptId(1),
        name: "get".to_owned(),
        span: span(),
        receiver: Some(Receiver::Readonly),
        method_type_parameters: 1,
        params: vec![
            RequirementType::SelfType,
            RequirementType::MethodParameter(0),
        ],
        return_ty: RequirementType::AssociatedProjection {
            witness: 0,
            associated: "Item".to_owned(),
        },
        witness_params: vec![RequirementWitnessParam {
            target: RequirementType::MethodParameter(0),
            concept: ConceptId(0),
            bindings: BTreeMap::new(),
            span: span(),
        }],
    };
    let mut get_impl = function(
        0,
        vec![
            local(0, Type::Int, false),
            local(1, Type::Parameter(0), false),
        ],
        Vec::new(),
        projection.clone(),
        Block {
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
                    arguments: vec![CallArgument::Value(copy(1, Type::Parameter(0)))],
                    witnesses: Vec::new(),
                },
                ty: projection.clone(),
                span: span(),
            })),
            span: span(),
        },
    );
    get_impl.type_parameters = 1;
    get_impl.receiver = Some(Receiver::Readonly);
    get_impl.witness_params.push(WitnessParam {
        target: Type::Parameter(0),
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
        span: span(),
    });

    let program = Program {
        concepts: vec![source, convert],
        requirements: vec![first, get],
        functions: vec![get_impl],
        witnesses: vec![Witness {
            id: WitnessId(0),
            concept: ConceptId(1),
            concrete: Type::Int,
            methods: BTreeMap::from([(RequirementId(1), FunctionId(0))]),
            associated: BTreeMap::new(),
            type_parameters: 0,
            prerequisites: Vec::new(),
        }],
        ..Program::default()
    };
    validate_program(&program).expect(
        "an unbound parameter proof preserves T.Item in both owner and method-bound projections",
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn nested_enum_pattern_alternatives_are_jointly_exhaustive() {
    let parse_error = Type::Nominal(TypeId(0), Vec::new());
    let result = Type::Nominal(TypeId(1), vec![Type::Int, parse_error.clone()]);
    let types = vec![
        TypeDef {
            id: TypeId(0),
            name: "ParseFloatError".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Enum {
                variants: vec![
                    VariantDef {
                        id: VariantId(0),
                        name: "InvalidSyntax".to_owned(),
                        payload: Vec::new(),
                        span: span(),
                    },
                    VariantDef {
                        id: VariantId(1),
                        name: "OutOfRange".to_owned(),
                        payload: Vec::new(),
                        span: span(),
                    },
                ],
            },
        },
        TypeDef {
            id: TypeId(1),
            name: "Result".to_owned(),
            span: span(),
            type_parameters: 2,
            kind: TypeDefKind::Enum {
                variants: vec![
                    VariantDef {
                        id: VariantId(0),
                        name: "Ok".to_owned(),
                        payload: vec![Type::Parameter(0)],
                        span: span(),
                    },
                    VariantDef {
                        id: VariantId(1),
                        name: "Err".to_owned(),
                        payload: vec![Type::Parameter(1)],
                        span: span(),
                    },
                ],
            },
        },
    ];
    let nested_error = |variant| Pattern::Variant {
        ty: TypeId(1),
        variant: VariantId(1),
        payload: vec![Pattern::Variant {
            ty: TypeId(0),
            variant: VariantId(variant),
            payload: Vec::new(),
        }],
    };
    let matcher = function(
        0,
        vec![local(0, result.clone(), false)],
        vec![local(1, Type::Int, false)],
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Match {
                    scrutinee: Box::new(copy(0, result)),
                    arms: vec![
                        MatchArm {
                            pattern: Pattern::Variant {
                                ty: TypeId(1),
                                variant: VariantId(0),
                                payload: vec![Pattern::Binding],
                            },
                            bindings: vec![LocalId(1)],
                            value: copy(1, Type::Int),
                        },
                        MatchArm {
                            pattern: nested_error(0),
                            bindings: Vec::new(),
                            value: constant(Constant::Int(0), Type::Int),
                        },
                        MatchArm {
                            pattern: nested_error(1),
                            bindings: Vec::new(),
                            value: constant(Constant::Int(0), Type::Int),
                        },
                    ],
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
    );
    validate_program(&Program {
        types,
        functions: vec![matcher],
        ..Program::default()
    })
    .expect("nested enum alternatives cover the enclosing Err payload jointly");
}

#[test]
#[allow(clippy::too_many_lines)]
fn contracts_use_explicit_receiver_arguments_lexical_bindings_and_refined_bases() {
    let true_contract = || Contract {
        code: "true".to_owned(),
        span: span(),
        expression: ContractExpr {
            kind: ContractExprKind::Constant(Constant::Bool(true)),
            span: span(),
        },
    };
    let types = vec![
        TypeDef {
            id: TypeId(0),
            name: "Price".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Refined {
                base: Type::Float,
                predicate: true_contract(),
            },
        },
        TypeDef {
            id: TypeId(1),
            name: "Order".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![
                    FieldDef {
                        name: "subtotal".to_owned(),
                        ty: Type::Nominal(TypeId(0), Vec::new()),
                        span: span(),
                    },
                    FieldDef {
                        name: "discount".to_owned(),
                        ty: Type::Nominal(TypeId(0), Vec::new()),
                        span: span(),
                    },
                ],
                invariant: Some(Contract {
                    code: "discount <= subtotal".to_owned(),
                    span: span(),
                    expression: ContractExpr {
                        kind: ContractExprKind::Binary(
                            loom_mir::BinaryOp::LessEqual,
                            Box::new(ContractExpr {
                                kind: ContractExprKind::Field(
                                    Box::new(ContractExpr {
                                        kind: ContractExprKind::Value(ContractValue::SelfValue),
                                        span: span(),
                                    }),
                                    1,
                                ),
                                span: span(),
                            }),
                            Box::new(ContractExpr {
                                kind: ContractExprKind::Field(
                                    Box::new(ContractExpr {
                                        kind: ContractExprKind::Value(ContractValue::SelfValue),
                                        span: span(),
                                    }),
                                    0,
                                ),
                                span: span(),
                            }),
                        ),
                        span: span(),
                    },
                }),
            },
        },
        TypeDef {
            id: TypeId(2),
            name: "Inner".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Enum {
                variants: vec![VariantDef {
                    id: VariantId(0),
                    name: "Value".to_owned(),
                    payload: vec![Type::Int],
                    span: span(),
                }],
            },
        },
        TypeDef {
            id: TypeId(3),
            name: "Outer".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Enum {
                variants: vec![VariantDef {
                    id: VariantId(0),
                    name: "Wrap".to_owned(),
                    payload: vec![Type::Nominal(TypeId(2), Vec::new())],
                    span: span(),
                }],
            },
        },
    ];

    let price = Type::Nominal(TypeId(0), Vec::new());
    let mut method = function(
        0,
        vec![
            local(0, Type::Nominal(TypeId(1), Vec::new()), true),
            local(1, price, false),
        ],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    method.receiver = Some(Receiver::Mutable);
    method.call_plan.requires.push(Contract {
        code: "explicit arg is price".to_owned(),
        span: span(),
        expression: ContractExpr {
            kind: ContractExprKind::Binary(
                loom_mir::BinaryOp::GreaterEqual,
                Box::new(ContractExpr {
                    kind: ContractExprKind::Value(ContractValue::Argument(0)),
                    span: span(),
                }),
                Box::new(ContractExpr {
                    kind: ContractExprKind::Constant(Constant::Float(0.0)),
                    span: span(),
                }),
            ),
            span: span(),
        },
    });

    let mut nested = function(
        1,
        vec![local(0, Type::Nominal(TypeId(3), Vec::new()), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    nested.call_plan.requires.push(Contract {
        code: "nested bindings".to_owned(),
        span: span(),
        expression: ContractExpr {
            kind: ContractExprKind::Match {
                scrutinee: Box::new(ContractExpr {
                    kind: ContractExprKind::Value(ContractValue::Argument(0)),
                    span: span(),
                }),
                arms: vec![ContractArm {
                    pattern: Pattern::Variant {
                        ty: TypeId(3),
                        variant: VariantId(0),
                        payload: vec![Pattern::Binding],
                    },
                    bindings: vec![Type::Nominal(TypeId(2), Vec::new())],
                    value: ContractExpr {
                        kind: ContractExprKind::Match {
                            scrutinee: Box::new(ContractExpr {
                                kind: ContractExprKind::Binding(0),
                                span: span(),
                            }),
                            arms: vec![ContractArm {
                                pattern: Pattern::Variant {
                                    ty: TypeId(2),
                                    variant: VariantId(0),
                                    payload: vec![Pattern::Binding],
                                },
                                bindings: vec![Type::Int],
                                value: ContractExpr {
                                    kind: ContractExprKind::Binary(
                                        loom_mir::BinaryOp::GreaterEqual,
                                        Box::new(ContractExpr {
                                            kind: ContractExprKind::Binding(1),
                                            span: span(),
                                        }),
                                        Box::new(ContractExpr {
                                            kind: ContractExprKind::Constant(Constant::Int(0)),
                                            span: span(),
                                        }),
                                    ),
                                    span: span(),
                                },
                            }],
                        },
                        span: span(),
                    },
                }],
            },
            span: span(),
        },
    });

    validate_program(&Program {
        types,
        functions: vec![method, nested],
        ..Program::default()
    })
    .expect("contracts preserve receiver/argument and lexical binding boundaries");
}

#[test]
fn phantom_type_arity_is_declared_and_checked() {
    let marker = TypeDef {
        id: TypeId(0),
        name: "Marker".to_owned(),
        span: span(),
        type_parameters: 1,
        kind: TypeDefKind::Record {
            fields: Vec::new(),
            invariant: None,
        },
    };
    let valid = function(
        0,
        Vec::new(),
        Vec::new(),
        Type::Nominal(TypeId(0), vec![Type::Int]),
        Block {
            statements: Vec::new(),
            tail: None,
            span: span(),
        },
    );
    let mut program = Program {
        types: vec![marker],
        functions: vec![valid],
        ..Program::default()
    };
    // The body mismatch is intentional and independent; the saturated type is
    // structurally valid. Unsaturating it adds a type-shape diagnostic.
    let baseline = validation_errors(&program);
    assert!(!baseline.as_slice().iter().any(|error| {
        error.path.ends_with("return_ty") && error.code == MirValidationCode::TypeMismatch
    }));
    program.functions[0].return_ty = Type::Nominal(TypeId(0), Vec::new());
    assert!(validation_errors(&program).contains(MirValidationCode::TypeMismatch));
}

#[test]
fn direct_by_value_nominal_cycle_is_rejected_at_the_checked_boundary() {
    let node = TypeId(0);
    let errors = validation_errors(&Program {
        types: vec![TypeDef {
            id: node,
            name: "Node".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "next".to_owned(),
                    ty: Type::Nominal(node, Vec::new()),
                    span: span(),
                }],
                invariant: None,
            },
        }],
        ..Program::default()
    });
    let error = errors
        .iter()
        .find(|error| error.code == MirValidationCode::RecursiveValueType)
        .expect("direct recursive storage must be rejected");
    assert_eq!(error.code.to_string(), "MirRecursiveValueType");
    assert_eq!(error.path, "types[0]");
    assert!(error.message.contains("`Node`"));
}

#[test]
fn mutual_tuple_nominal_cycle_is_rejected_as_one_component() {
    let left = TypeId(0);
    let right = TypeId(1);
    let errors = validation_errors(&Program {
        types: vec![
            TypeDef {
                id: left,
                name: "Left".to_owned(),
                span: span(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "right".to_owned(),
                        ty: Type::Tuple(vec![Type::Int, Type::Nominal(right, Vec::new())]),
                        span: span(),
                    }],
                    invariant: None,
                },
            },
            TypeDef {
                id: right,
                name: "Right".to_owned(),
                span: span(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "left".to_owned(),
                        ty: Type::Nominal(left, Vec::new()),
                        span: span(),
                    }],
                    invariant: None,
                },
            },
        ],
        ..Program::default()
    });
    let recursive = errors
        .iter()
        .filter(|error| error.code == MirValidationCode::RecursiveValueType)
        .collect::<Vec<_>>();
    assert_eq!(recursive.len(), 1, "{errors:?}");
    assert_eq!(recursive[0].path, "types[0]");
    assert!(recursive[0].message.contains("`Left`, `Right`"));
}

#[test]
fn nominal_arguments_are_conservative_by_value_edges() {
    let carrier = TypeId(0);
    let recursive = TypeId(1);
    let errors = validation_errors(&Program {
        types: vec![
            TypeDef {
                id: carrier,
                name: "Carrier".to_owned(),
                span: span(),
                type_parameters: 1,
                kind: TypeDefKind::Record {
                    fields: Vec::new(),
                    invariant: None,
                },
            },
            TypeDef {
                id: recursive,
                name: "Recursive".to_owned(),
                span: span(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "carrier".to_owned(),
                        ty: Type::Nominal(carrier, vec![Type::Nominal(recursive, Vec::new())]),
                        span: span(),
                    }],
                    invariant: None,
                },
            },
        ],
        ..Program::default()
    });
    assert!(errors.contains(MirValidationCode::RecursiveValueType));
}

#[test]
fn task_outcome_payload_remains_a_by_value_edge() {
    let outcome = TypeId(0);
    let errors = validation_errors(&Program {
        types: vec![TypeDef {
            id: outcome,
            name: "OutcomeLoop".to_owned(),
            span: span(),
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "outcome".to_owned(),
                    ty: Type::TaskOutcome(Box::new(Type::Nominal(outcome, Vec::new()))),
                    span: span(),
                }],
                invariant: None,
            },
        }],
        ..Program::default()
    });
    assert!(errors.contains(MirValidationCode::RecursiveValueType));
}

#[test]
fn indirect_carriers_break_recursive_nominal_storage() {
    let text_map = TypeId(0);
    let list_node = TypeId(1);
    let map_node = TypeId(2);
    let task_node = TypeId(3);
    let view_node = TypeId(4);
    let record = |id, name: &str, field: Type| TypeDef {
        id,
        name: name.to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "next".to_owned(),
                ty: field,
                span: span(),
            }],
            invariant: None,
        },
    };
    let program = Program {
        types: vec![
            TypeDef {
                id: text_map,
                name: "TextMap".to_owned(),
                span: span(),
                type_parameters: 1,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "raw".to_owned(),
                        ty: Type::Int,
                        span: span(),
                    }],
                    invariant: None,
                },
            },
            record(
                list_node,
                "ListNode",
                Type::List(Box::new(Type::Nominal(list_node, Vec::new()))),
            ),
            record(
                map_node,
                "MapNode",
                Type::Nominal(text_map, vec![Type::Nominal(map_node, Vec::new())]),
            ),
            record(
                task_node,
                "TaskNode",
                Type::Task(Box::new(Type::Nominal(task_node, Vec::new()))),
            ),
            record(
                view_node,
                "ViewNode",
                Type::View {
                    mutable: false,
                    concept: ConceptId(0),
                    bindings: BTreeMap::from([(
                        "Item".to_owned(),
                        Type::Nominal(view_node, Vec::new()),
                    )]),
                },
            ),
        ],
        concepts: vec![ConceptDef {
            id: ConceptId(0),
            module: "test".to_owned(),
            name: "Source".to_owned(),
            span: span(),
            identity: None,
            dynamic: true,
            associated_types: vec![AssociatedTypeDef {
                name: "Item".to_owned(),
                span: span(),
            }],
            requirements: Vec::new(),
        }],
        prelude: PreludeIds {
            text_map: Some(text_map),
            ..PreludeIds::default()
        },
        ..Program::default()
    };
    validate_program(&program).expect("all four recursive edges cross explicit indirection");
}

const NON_REGULAR_VALIDATION_CHILD_ENV: &str = "LOOM_MIR_NON_REGULAR_VALIDATION_CHILD";

fn non_regular_spiral_definition(spiral: TypeId) -> TypeDef {
    TypeDef {
        id: spiral,
        name: "Spiral".to_owned(),
        span: span(),
        type_parameters: 1,
        kind: TypeDefKind::Enum {
            variants: vec![
                VariantDef {
                    id: VariantId(0),
                    name: "Done".to_owned(),
                    payload: vec![Type::Parameter(0)],
                    span: span(),
                },
                VariantDef {
                    id: VariantId(1),
                    name: "Next".to_owned(),
                    payload: vec![Type::Nominal(
                        spiral,
                        vec![Type::Tuple(vec![Type::Parameter(0), Type::Parameter(0)])],
                    )],
                    span: span(),
                },
            ],
        },
    }
}

fn non_regular_done(spiral: TypeId) -> Expr {
    expr(
        ExprKind::Variant {
            ty: spiral,
            type_arguments: vec![Type::Int],
            variant: VariantId(0),
            payload: vec![constant(Constant::Int(0), Type::Int)],
        },
        Type::Nominal(spiral, vec![Type::Int]),
    )
}

#[test]
fn non_regular_generic_validation_finishes_within_the_resource_gate() {
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "non_regular_generic_validation_child",
            "--nocapture",
        ])
        .env(NON_REGULAR_VALIDATION_CHILD_ENV, "1")
        .spawn()
        .expect("spawn non-regular validation child");
    let status = child
        .wait_timeout(Duration::from_secs(15))
        .expect("wait for non-regular validation child");
    let Some(status) = status else {
        child.kill().expect("kill timed-out validation child");
        child.wait().expect("reap timed-out validation child");
        panic!("non-regular generic MIR validation exceeded 15 seconds");
    };
    assert!(status.success(), "non-regular validation child failed");
}

#[test]
fn non_regular_generic_validation_child() {
    if std::env::var_os(NON_REGULAR_VALIDATION_CHILD_ENV).is_none() {
        return;
    }

    let spiral = TypeId(0);
    let definition = non_regular_spiral_definition(spiral);
    let checked_main = function(
        0,
        Vec::new(),
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(non_regular_done(spiral)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expr(
                        ExprKind::Binary(
                            BinaryOp::Equal,
                            Box::new(non_regular_done(spiral)),
                            Box::new(non_regular_done(spiral)),
                        ),
                        Type::Bool,
                    )),
                    span: span(),
                },
            ],
            tail: None,
            span: span(),
        },
    );
    let errors = Program {
        types: vec![definition.clone()],
        functions: vec![checked_main],
        exports: BTreeMap::from([("main".to_owned(), FunctionId(0))]),
        ..Program::default()
    }
    .into_checked()
    .expect_err("a non-regular by-value schema has infinite storage");
    assert!(errors.contains(MirValidationCode::RecursiveValueType));

    let incomplete_match = expr(
        ExprKind::Match {
            scrutinee: Box::new(non_regular_done(spiral)),
            arms: vec![MatchArm {
                pattern: Pattern::Variant {
                    ty: spiral,
                    variant: VariantId(0),
                    payload: vec![Pattern::Wildcard],
                },
                bindings: Vec::new(),
                value: constant(Constant::Unit, Type::Unit),
            }],
        },
        Type::Unit,
    );
    let invalid = Program {
        types: vec![definition],
        functions: vec![function(
            0,
            Vec::new(),
            Vec::new(),
            Type::Unit,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(incomplete_match)),
                span: span(),
            },
        )],
        ..Program::default()
    };
    let errors = validate_program(&invalid).expect_err("the match intentionally omits Next");
    assert!(errors.contains(MirValidationCode::RecursiveValueType));
    assert!(errors.contains(MirValidationCode::PatternShape));
}

const DEEP_WITNESS_CHILD_ENV: &str = "LOOM_MIR_DEEP_WITNESS_CHILD";

#[test]
fn recursive_witness_proof_fails_closed_without_process_abort() {
    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "recursive_witness_proof_child", "--nocapture"])
        .env(DEEP_WITNESS_CHILD_ENV, "1")
        .output()
        .expect("spawn deep proof child");
    assert!(
        output.status.success(),
        "deep proof child aborted: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("deep-proof-failed-closed"),
        "child did not reach the fail-closed assertion"
    );
}

#[test]
fn recursive_witness_proof_child() {
    if std::env::var_os(DEEP_WITNESS_CHILD_ENV).is_none() {
        return;
    }
    let mut program = conditional_concept_program();
    let mut proof = WitnessRef::Concrete(WitnessId(0));
    for _ in 0..20_000 {
        proof = WitnessRef::Apply {
            witness: WitnessId(1),
            arguments: vec![proof],
        };
    }
    let ExprKind::Call {
        target: CallTarget::StaticConcept { witness, .. },
        ..
    } = &mut program.functions[2].body.tail.as_mut().expect("call").kind
    else {
        panic!("expected static call");
    };
    *witness = proof;
    let errors = validate_program(&program).expect_err("deep proof must be rejected");
    assert!(errors.contains(MirValidationCode::NestingLimit));
    println!("deep-proof-failed-closed");
    // Recursive drop of hostile unchecked input is outside the checked
    // artifact boundary and can itself consume the native stack.
    std::mem::forget(program);

    let mut deep_type = Type::Int;
    for _ in 0..20_000 {
        deep_type = Type::Nominal(TypeId(0), vec![deep_type]);
    }
    let deep_program = Program {
        types: vec![TypeDef {
            id: TypeId(0),
            name: "Box".to_owned(),
            span: span(),
            type_parameters: 1,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "value".to_owned(),
                    ty: Type::Parameter(0),
                    span: span(),
                }],
                invariant: None,
            },
        }],
        functions: vec![function(
            0,
            Vec::new(),
            Vec::new(),
            deep_type,
            Block::default(),
        )],
        ..Program::default()
    };
    let errors = validate_program(&deep_program).expect_err("deep type must be rejected");
    assert!(errors.contains(MirValidationCode::NestingLimit));
    std::mem::forget(deep_program);
}

#[test]
fn checked_boundary_rejects_uninitialized_double_init_and_use_after_move() {
    let checked = function(
        0,
        Vec::new(),
        vec![local(0, Type::Int, true)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(copy(0, Type::Int)),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(1), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: constant(Constant::Int(2), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr {
                        id: ExprId::UNASSIGNED,
                        kind: ExprKind::Move(Place::local(LocalId(0))),
                        ty: Type::Int,
                        span: span(),
                    }),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(copy(0, Type::Int)),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    assert!(
        validation_errors(&Program {
            functions: vec![checked],
            ..Program::default()
        })
        .contains(MirValidationCode::LocalState)
    );
}

#[test]
fn borrowed_view_carriers_protect_the_owner() {
    let concept = ConceptDef {
        id: ConceptId(0),
        module: "test".to_owned(),
        name: "Viewable".to_owned(),
        span: span(),
        identity: None,
        dynamic: true,
        associated_types: Vec::new(),
        requirements: Vec::new(),
    };
    let view = Type::View {
        mutable: true,
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
    };
    let make_view = |token| Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::MakeView {
            value: Box::new(copy(0, Type::Int)),
            writeback: Some(Place::local(LocalId(0))),
            witness: WitnessRef::Concrete(WitnessId(0)),
            mutable: true,
            token,
        },
        ty: view.clone(),
        span: span(),
    };
    let borrower = function(
        0,
        vec![local(0, Type::Int, true)],
        vec![local(1, view.clone(), false), local(2, view.clone(), false)],
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: make_view(7),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(copy(1, view.clone())),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(2),
                        value: make_view(7),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr {
                        id: ExprId::UNASSIGNED,
                        kind: ExprKind::Move(Place::local(LocalId(0))),
                        ty: Type::Int,
                        span: span(),
                    }),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let escaping = function(
        1,
        vec![local(0, Type::Int, true)],
        Vec::new(),
        view.clone(),
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(make_view(8))),
            span: span(),
        },
    );
    let errors = validation_errors(&Program {
        concepts: vec![concept],
        functions: vec![borrower, escaping],
        witnesses: vec![Witness {
            id: WitnessId(0),
            concept: ConceptId(0),
            concrete: Type::Int,
            methods: BTreeMap::new(),
            associated: BTreeMap::new(),
            type_parameters: 0,
            prerequisites: Vec::new(),
        }],
        ..Program::default()
    });
    assert!(errors.contains(MirValidationCode::BorrowShape));
}

#[test]
fn borrowed_views_cannot_hide_in_value_carriers_or_naked_evaluate() {
    let view = view_type(true);
    let record = TypeDef {
        id: TypeId(0),
        name: "ViewBox".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "value".to_owned(),
                ty: view.clone(),
                span: span(),
            }],
            invariant: None,
        },
    };
    let enumeration = TypeDef {
        id: TypeId(1),
        name: "MaybeView".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Enum {
            variants: vec![VariantDef {
                id: VariantId(0),
                name: "Some".to_owned(),
                payload: vec![view.clone()],
                span: span(),
            }],
        },
    };
    let make = |token| borrowed_view(Place::local(LocalId(0)), 0, token, true);
    let expressions = vec![
        make(1),
        expr(
            ExprKind::Tuple(vec![make(2)]),
            Type::Tuple(vec![view.clone()]),
        ),
        expr(
            ExprKind::List(vec![make(3)]),
            Type::List(Box::new(view.clone())),
        ),
        expr(
            ExprKind::Record {
                ty: TypeId(0),
                type_arguments: Vec::new(),
                fields: vec![make(4)],
                construction: ConstructionMode::Plain,
            },
            Type::Nominal(TypeId(0), Vec::new()),
        ),
        expr(
            ExprKind::Variant {
                ty: TypeId(1),
                type_arguments: Vec::new(),
                variant: VariantId(0),
                payload: vec![make(5)],
            },
            Type::Nominal(TypeId(1), Vec::new()),
        ),
    ];
    let borrower = function(
        0,
        vec![local(0, Type::Int, true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: expressions
                .into_iter()
                .map(|expression| Statement {
                    kind: StatementKind::Evaluate(expression),
                    span: span(),
                })
                .collect(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let errors = validation_errors(&Program {
        concepts: vec![empty_dyn_concept()],
        types: vec![record, enumeration],
        functions: vec![borrower],
        witnesses: vec![empty_witness(0, Type::Int)],
        ..Program::default()
    });
    assert_eq!(
        errors
            .iter()
            .filter(|error| {
                error.code == MirValidationCode::BorrowShape
                    && error.message.contains("direct value expression")
            })
            .count(),
        5
    );
}

#[test]
fn reborrowed_views_are_direct_sync_call_arguments_only() {
    let view = view_type(false);
    let sink = function(
        0,
        vec![local(0, view.clone(), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let reborrow = |token| {
        expr(
            ExprKind::ReborrowView {
                owner: Place::local(LocalId(0)),
                mutable: false,
                token,
            },
            view.clone(),
        )
    };
    let valid = function(
        1,
        vec![local(0, view.clone(), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(expr(
                ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(0)),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(reborrow(1))],
                    witnesses: Vec::new(),
                },
                Type::Unit,
            ))),
            span: span(),
        },
    );
    validate_program(&Program {
        concepts: vec![empty_dyn_concept()],
        functions: vec![sink.clone(), valid],
        ..Program::default()
    })
    .expect("a direct reborrow into a synchronous call is valid");

    let invalid = function(
        1,
        vec![local(0, view.clone(), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(reborrow(2)),
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    assert!(
        validation_errors(&Program {
            concepts: vec![empty_dyn_concept()],
            functions: vec![sink, invalid],
            ..Program::default()
        })
        .contains(MirValidationCode::BorrowShape)
    );
}

#[test]
fn borrowed_view_async_rejection_uses_the_real_callee() {
    let view = view_type(true);
    let mut asynchronous = function(
        0,
        vec![local(0, view, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    asynchronous.is_async = true;
    let caller = function(
        1,
        vec![local(0, Type::Int, true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(expr(
                    ExprKind::Call {
                        target: CallTarget::Direct(FunctionId(0)),
                        type_arguments: Vec::new(),
                        arguments: vec![CallArgument::Value(borrowed_view(
                            Place::local(LocalId(0)),
                            0,
                            1,
                            true,
                        ))],
                        witnesses: Vec::new(),
                    },
                    // Deliberately corrupt the declared result so this test
                    // cannot pass by checking Type::Task on the call node.
                    Type::Unit,
                )),
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let errors = validation_errors(&Program {
        concepts: vec![empty_dyn_concept()],
        functions: vec![asynchronous, caller],
        witnesses: vec![empty_witness(0, Type::Int)],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::BorrowShape
            && error.message.contains("target is not proven synchronous")
    }));
}

#[test]
fn borrowed_view_dynamic_dispatch_checks_the_witness_target() {
    let requirement = RequirementDef {
        id: RequirementId(0),
        concept: ConceptId(0),
        name: "inspect".to_owned(),
        span: span(),
        receiver: Some(Receiver::Readonly),
        method_type_parameters: 0,
        params: vec![RequirementType::SelfType],
        return_ty: RequirementType::Unit,
        witness_params: Vec::new(),
    };
    let concept = ConceptDef {
        id: ConceptId(0),
        module: "test".to_owned(),
        name: "Viewable".to_owned(),
        span: span(),
        identity: None,
        dynamic: true,
        associated_types: Vec::new(),
        requirements: vec![RequirementId(0)],
    };
    let mut method = function(
        0,
        vec![local(0, Type::Int, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    method.receiver = Some(Receiver::Readonly);
    method.is_async = true;
    let caller = function(
        1,
        vec![local(0, Type::Int, false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(expr(
                ExprKind::Call {
                    target: CallTarget::Dynamic {
                        requirement: RequirementId(0),
                    },
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(borrowed_view(
                        Place::local(LocalId(0)),
                        0,
                        1,
                        false,
                    ))],
                    witnesses: Vec::new(),
                },
                Type::Unit,
            ))),
            span: span(),
        },
    );
    let mut methods = BTreeMap::new();
    methods.insert(RequirementId(0), FunctionId(0));
    let witness = Witness {
        id: WitnessId(0),
        concept: ConceptId(0),
        concrete: Type::Int,
        methods,
        associated: BTreeMap::new(),
        type_parameters: 0,
        prerequisites: Vec::new(),
    };
    let errors = validation_errors(&Program {
        concepts: vec![concept],
        requirements: vec![requirement],
        functions: vec![method, caller],
        witnesses: vec![witness],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::BorrowShape
            && error.message.contains("target is not proven synchronous")
    }));
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::WitnessShape
            && error.message.contains("async concept requirements")
    }));
}

#[test]
#[allow(clippy::too_many_lines)]
fn borrowed_view_places_use_projection_prefix_overlap() {
    let pair = Type::Nominal(TypeId(0), Vec::new());
    let pair_def = TypeDef {
        id: TypeId(0),
        name: "Pair".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![
                FieldDef {
                    name: "left".to_owned(),
                    ty: Type::Int,
                    span: span(),
                },
                FieldDef {
                    name: "right".to_owned(),
                    ty: Type::Int,
                    span: span(),
                },
            ],
            invariant: None,
        },
    };
    let view = view_type(true);
    let sink = function(
        0,
        vec![local(0, view.clone(), false), local(1, view.clone(), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let call = |left: Expr, right: Expr| {
        expr(
            ExprKind::Call {
                target: CallTarget::Direct(FunctionId(0)),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::Value(left), CallArgument::Value(right)],
                witnesses: Vec::new(),
            },
            Type::Unit,
        )
    };
    let sibling_caller = function(
        1,
        vec![local(0, pair.clone(), true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(call(
                borrowed_view(
                    Place {
                        local: LocalId(0),
                        projection: vec![0],
                    },
                    0,
                    1,
                    true,
                ),
                borrowed_view(
                    Place {
                        local: LocalId(0),
                        projection: vec![1],
                    },
                    0,
                    2,
                    true,
                ),
            ))),
            span: span(),
        },
    );
    validate_program(&Program {
        concepts: vec![empty_dyn_concept()],
        types: vec![pair_def.clone()],
        functions: vec![sink.clone(), sibling_caller],
        witnesses: vec![empty_witness(0, Type::Int)],
        ..Program::default()
    })
    .expect("mutable borrows of sibling fields do not overlap");

    let parent = expr(
        ExprKind::MakeView {
            value: Box::new(copy_place(Place::local(LocalId(0)), pair.clone())),
            writeback: Some(Place::local(LocalId(0))),
            witness: WitnessRef::Concrete(WitnessId(1)),
            mutable: true,
            token: 3,
        },
        view.clone(),
    );
    let child = borrowed_view(
        Place {
            local: LocalId(0),
            projection: vec![0],
        },
        0,
        4,
        true,
    );
    let overlapping_caller = function(
        1,
        vec![local(0, pair.clone(), true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(call(parent, child))),
            span: span(),
        },
    );
    assert!(
        validation_errors(&Program {
            concepts: vec![empty_dyn_concept()],
            types: vec![pair_def],
            functions: vec![sink, overlapping_caller],
            witnesses: vec![empty_witness(0, Type::Int), empty_witness(1, pair)],
            ..Program::default()
        })
        .contains(MirValidationCode::BorrowShape)
    );
}

#[test]
fn borrowed_make_view_copy_must_match_its_writeback_place() {
    let pair = Type::Nominal(TypeId(0), Vec::new());
    let pair_def = TypeDef {
        id: TypeId(0),
        name: "Pair".to_owned(),
        span: span(),
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![
                FieldDef {
                    name: "left".to_owned(),
                    ty: Type::Int,
                    span: span(),
                },
                FieldDef {
                    name: "right".to_owned(),
                    ty: Type::Int,
                    span: span(),
                },
            ],
            invariant: None,
        },
    };
    let view = view_type(true);
    let sink = function(
        0,
        vec![local(0, view.clone(), false)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    let left = Place {
        local: LocalId(0),
        projection: vec![0],
    };
    let right = Place {
        local: LocalId(0),
        projection: vec![1],
    };
    let mismatched = expr(
        ExprKind::MakeView {
            value: Box::new(copy_place(left, Type::Int)),
            writeback: Some(right),
            witness: WitnessRef::Concrete(WitnessId(0)),
            mutable: true,
            token: 1,
        },
        view,
    );
    let caller = function(
        1,
        vec![local(0, pair, true)],
        Vec::new(),
        Type::Unit,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(expr(
                ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(0)),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(mismatched)],
                    witnesses: Vec::new(),
                },
                Type::Unit,
            ))),
            span: span(),
        },
    );
    let errors = validation_errors(&Program {
        concepts: vec![empty_dyn_concept()],
        types: vec![pair_def],
        functions: vec![sink, caller],
        witnesses: vec![empty_witness(0, Type::Int)],
        ..Program::default()
    });
    assert!(errors.iter().any(|error| {
        error.code == MirValidationCode::BorrowShape
            && error.message.contains("copy exactly its writeback place")
    }));
}

#[test]
fn generic_calls_and_constructors_share_one_strict_substitution() {
    let pair = TypeDef {
        id: TypeId(0),
        name: "Pair".to_owned(),
        span: span(),
        type_parameters: 1,
        kind: TypeDefKind::Record {
            fields: vec![
                FieldDef {
                    name: "left".to_owned(),
                    ty: Type::Parameter(0),
                    span: span(),
                },
                FieldDef {
                    name: "right".to_owned(),
                    ty: Type::Parameter(0),
                    span: span(),
                },
            ],
            invariant: None,
        },
    };
    let mut same = function(
        0,
        vec![
            local(0, Type::Parameter(0), false),
            local(1, Type::Parameter(0), false),
        ],
        Vec::new(),
        Type::Parameter(0),
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(copy(0, Type::Parameter(0)))),
            span: span(),
        },
    );
    same.type_parameters = 1;
    let bad_record = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Record {
            ty: TypeId(0),
            type_arguments: vec![Type::Int],
            fields: vec![
                constant(Constant::Int(1), Type::Int),
                constant(Constant::Text("bad".to_owned()), Type::Text),
            ],
            construction: ConstructionMode::Plain,
        },
        ty: Type::Nominal(TypeId(0), vec![Type::Int]),
        span: span(),
    };
    let bad_call = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Call {
            target: CallTarget::Direct(FunctionId(0)),
            type_arguments: vec![Type::Int],
            arguments: vec![
                CallArgument::Value(constant(Constant::Int(1), Type::Int)),
                CallArgument::Value(constant(Constant::Text("bad".to_owned()), Type::Text)),
            ],
            witnesses: Vec::new(),
        },
        ty: Type::Int,
        span: span(),
    };
    let caller = function(
        1,
        Vec::new(),
        Vec::new(),
        Type::Unit,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(bad_record),
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(bad_call),
                    span: span(),
                },
            ],
            tail: Some(Box::new(constant(Constant::Unit, Type::Unit))),
            span: span(),
        },
    );
    assert!(
        validation_errors(&Program {
            types: vec![pair],
            functions: vec![same, caller],
            ..Program::default()
        })
        .contains(MirValidationCode::TypeMismatch)
    );
}

#[test]
fn match_must_be_exhaustive_and_nested_return_flow_is_never() {
    let non_exhaustive = function(
        0,
        vec![local(0, Type::Bool, false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Match {
                    scrutinee: Box::new(copy(0, Type::Bool)),
                    arms: vec![loom_mir::MatchArm {
                        pattern: Pattern::Constant(Constant::Bool(true)),
                        bindings: Vec::new(),
                        value: constant(Constant::Int(1), Type::Int),
                    }],
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
    );
    assert!(
        validation_errors(&Program {
            functions: vec![non_exhaustive],
            ..Program::default()
        })
        .contains(MirValidationCode::PatternShape)
    );

    let return_block = || Block {
        statements: vec![Statement {
            kind: StatementKind::Return(Some(constant(Constant::Int(1), Type::Int))),
            span: span(),
        }],
        tail: None,
        span: span(),
    };
    let nested_return = function(
        0,
        vec![local(0, Type::Bool, false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::If {
                        condition: Box::new(copy(0, Type::Bool)),
                        then_branch: return_block(),
                        else_branch: return_block(),
                    },
                    ty: Type::Int,
                    span: span(),
                }),
                span: span(),
            }],
            tail: None,
            span: span(),
        },
    );
    validate_program(&Program {
        functions: vec![nested_return],
        ..Program::default()
    })
    .expect("all nested branches return, so the enclosing block diverges");
}

#[test]
fn checked_mir_rejects_an_unreachable_match_arm() {
    let arm = |value: bool, result: i64| MatchArm {
        pattern: Pattern::Constant(Constant::Bool(value)),
        bindings: Vec::new(),
        value: constant(Constant::Int(result), Type::Int),
    };
    let duplicate = function(
        0,
        vec![local(0, Type::Bool, false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Match {
                    scrutinee: Box::new(copy(0, Type::Bool)),
                    arms: vec![arm(true, 1), arm(true, 2), arm(false, 0)],
                },
                ty: Type::Int,
                span: span(),
            })),
            span: span(),
        },
    );
    let errors = validation_errors(&Program {
        functions: vec![duplicate],
        ..Program::default()
    });
    assert!(errors.as_slice().iter().any(|error| {
        error.code == MirValidationCode::PatternShape
            && error.message.contains("match arm is unreachable")
    }));
}

fn suspension_function(live_locals: Vec<LocalId>) -> Function {
    let mut function = function(
        0,
        vec![local(0, Type::Int, false)],
        vec![local(1, Type::Int, false)],
        Type::Int,
        Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: constant(Constant::Int(7), Type::Int),
                    },
                    span: span(),
                },
                Statement {
                    kind: StatementKind::Evaluate(sleep_await(1)),
                    span: span(),
                },
            ],
            tail: Some(Box::new(copy(1, Type::Int))),
            span: span(),
        },
    );
    function.is_async = true;
    function.suspension_points = vec![SuspensionPoint {
        state: 1,
        span: span(),
        live_locals,
    }];
    function
}

#[test]
fn checked_mir_accepts_exact_suspension_liveness() {
    validate_program(&Program {
        functions: vec![suspension_function(vec![LocalId(1)])],
        ..Program::default()
    })
    .expect("exact suspension liveness validates");
}

fn exit_contract_suspension_function(live_locals: Vec<LocalId>) -> Function {
    let mut function = function(
        0,
        vec![local(0, Type::Int, false), local(1, Type::Int, false)],
        Vec::new(),
        Type::Int,
        Block {
            statements: vec![Statement {
                kind: StatementKind::Evaluate(sleep_await(1)),
                span: span(),
            }],
            tail: Some(Box::new(constant(Constant::Int(7), Type::Int))),
            span: span(),
        },
    );
    function.is_async = true;
    function.call_plan.ensures.push(Contract {
        code: "postcondition".to_owned(),
        span: span(),
        expression: ContractExpr {
            kind: ContractExprKind::Binary(
                BinaryOp::GreaterEqual,
                Box::new(ContractExpr {
                    kind: ContractExprKind::Value(ContractValue::Result),
                    span: span(),
                }),
                Box::new(ContractExpr {
                    kind: ContractExprKind::Value(ContractValue::OldArgument(1)),
                    span: span(),
                }),
            ),
            span: span(),
        },
    });
    function.suspension_points = vec![SuspensionPoint {
        state: 1,
        span: span(),
        live_locals,
    }];
    function
}

#[test]
fn checked_mir_recomputes_exact_exit_contract_parameter_liveness() {
    validate_program(&Program {
        functions: vec![exit_contract_suspension_function(vec![LocalId(1)])],
        ..Program::default()
    })
    .expect("the one postcondition parameter is exact suspension state");

    let stale = validation_errors(&Program {
        functions: vec![exit_contract_suspension_function(vec![
            LocalId(0),
            LocalId(1),
        ])],
        ..Program::default()
    });
    assert!(stale.as_slice().iter().any(|error| {
        error.code == MirValidationCode::SuspensionShape
            && error.message.contains("live locals must be [LocalId(1)]")
    }));
}

#[test]
fn checked_mir_rejects_stale_or_unstable_suspension_liveness() {
    let stale = validation_errors(&Program {
        functions: vec![suspension_function(vec![LocalId(0), LocalId(1)])],
        ..Program::default()
    });
    assert!(stale.as_slice().iter().any(|error| {
        error.code == MirValidationCode::SuspensionShape
            && error.message.contains("live locals must be")
    }));

    let unstable = validation_errors(&Program {
        functions: vec![suspension_function(vec![LocalId(1), LocalId(1)])],
        ..Program::default()
    });
    assert!(unstable.as_slice().iter().any(|error| {
        error.code == MirValidationCode::SuspensionShape
            && error.message.contains("strictly sorted and unique")
    }));
}

#[test]
fn checked_mir_rejects_missing_or_non_dense_suspension_metadata() {
    let mut missing = suspension_function(vec![LocalId(1)]);
    missing.suspension_points.clear();
    let missing = validation_errors(&Program {
        functions: vec![missing],
        ..Program::default()
    });
    assert!(missing.contains(MirValidationCode::SuspensionShape));

    let mut non_dense = suspension_function(vec![LocalId(1)]);
    non_dense.suspension_points[0].state = 2;
    let non_dense = validation_errors(&Program {
        functions: vec![non_dense],
        ..Program::default()
    });
    assert!(non_dense.as_slice().iter().any(|error| {
        error.code == MirValidationCode::SuspensionShape
            && error.message.contains("dense state order")
    }));
}

#[test]
fn artifact_rejects_wire_nesting_before_recursive_decode() {
    let mut hostile = vec![b'['; 20_000];
    hostile.extend(std::iter::repeat_n(b']', 20_000));
    assert!(matches!(
        decode_interpreted_artifact(&hostile),
        Err(ArtifactError::Malformed(_))
    ));
}
