use std::collections::BTreeMap;
use std::process::Command;

use loom_core::Span;
use loom_mir::{
    ArtifactError, AssociatedTypeDef, Block, CallArgument, CallPlan, CallTarget, CheckedProgram,
    ConceptDef, ConceptId, Constant, ConstructionMode, Contract, ContractArm, ContractExpr,
    ContractExprKind, ContractValue, Expr, ExprId, ExprKind, FieldDef, Function, FunctionId,
    INTERPRETED_ARTIFACT_VERSION, LocalDecl, LocalId, MatchArm, MirValidationCode, Pattern, Place,
    PreludeIds, Program, Receiver, RequirementDef, RequirementId, RequirementType,
    RequirementWitnessParam, Statement, StatementKind, SuspensionPoint, Type, TypeDef, TypeDefKind,
    TypeId, VariantDef, VariantId, Witness, WitnessId, WitnessParam, WitnessRef,
    decode_interpreted_artifact, decode_interpreted_executable_artifact,
    encode_interpreted_artifact, encode_interpreted_executable_artifact, validate_program,
};

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

fn copy(id: u32, ty: Type) -> Expr {
    expr(ExprKind::Copy(Place::local(LocalId(id))), ty)
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
        name: "Viewable".to_owned(),
        span: span(),
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
                target: CallTarget::Builtin(loom_mir::Builtin::IsFinite),
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
            ExprKind::WaitFd {
                descriptor: Box::new(constant(Constant::Int(1), Type::Int)),
                writable: false,
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
        (0..50).collect::<Vec<_>>()
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
        name: "Display".to_owned(),
        span: span(),
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
    assert!(errors.contains(MirValidationCode::ProjectedMove));
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
                fields: Vec::new(),
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
            parse_float_error: None,
            parse_int_error: None,
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
    validate_program(&program).expect("resource close through inout place");
}

fn float_program(bits: u64) -> CheckedProgram {
    Program {
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
    }
    .into_checked()
    .expect("valid floating-point artifact fixture")
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
fn interpreted_executable_artifact_round_trips_and_validates_its_fixed_entry() {
    let program = float_program(1.0_f64.to_bits());
    let bytes =
        encode_interpreted_executable_artifact(&program, "main").expect("encode executable");
    let (decoded, entry) =
        decode_interpreted_executable_artifact(&bytes).expect("decode executable");
    assert_eq!(entry, "main");
    assert!(decoded.exports.contains_key(&entry));

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
fn all_nan_payloads_encode_to_identical_bytes() {
    let left = encode_interpreted_artifact(&float_program(0x7ff0_0000_0000_0001)).expect("left");
    let right = encode_interpreted_artifact(&float_program(0xfff8_0000_0000_1234)).expect("right");
    assert_eq!(left, right);
}

#[test]
fn artifact_rejects_version_mismatch_before_program_decode() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["version"] = serde_json::json!(99);
    value["program"] = serde_json::json!("future incompatible body");
    let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
        .expect_err("version must fail first");
    assert!(matches!(
        error,
        ArtifactError::VersionMismatch {
            expected,
            found: 99
        } if expected == INTERPRETED_ARTIFACT_VERSION
    ));
}

#[test]
fn artifact_rejects_pre_structured_values_version_fourteen() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["version"] = serde_json::json!(14);
    let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
        .expect_err("version 14 lacks structured standard value shapes");
    assert!(matches!(
        error,
        ArtifactError::VersionMismatch {
            expected,
            found: 14
        } if expected == INTERPRETED_ARTIFACT_VERSION
    ));
}

#[test]
fn artifact_rejects_pre_expression_identity_version_fifteen_before_body_decode() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["version"] = serde_json::json!(15);
    value["program"] = serde_json::json!("version 15 has no expression identities");
    let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
        .expect_err("version 15 must fail at the header boundary");
    assert!(matches!(
        error,
        ArtifactError::VersionMismatch {
            expected,
            found: 15
        } if expected == INTERPRETED_ARTIFACT_VERSION
    ));
}

#[test]
fn artifact_rejects_pre_witness_segmentation_version_sixteen_before_body_decode() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["version"] = serde_json::json!(16);
    value["program"] = serde_json::json!("version 16 has no witness proof segmentation");
    let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
        .expect_err("version 16 must fail at the header boundary");
    assert!(matches!(
        error,
        ArtifactError::VersionMismatch {
            expected,
            found: 16
        } if expected == INTERPRETED_ARTIFACT_VERSION
    ));
}

#[test]
fn artifact_version_seventeen_requires_explicit_witness_segmentation() {
    let bytes = encode_interpreted_artifact(&float_program(1.0_f64.to_bits())).expect("encode");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["program"]["functions"]
        .as_array_mut()
        .and_then(|functions| functions.first_mut())
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|function| function.remove("witness_prefix_count"))
        .expect("encoded function witness segmentation field");
    let error = decode_interpreted_artifact(&serde_json::to_vec(&value).expect("json"))
        .expect_err("version 17 function segmentation is required");
    assert!(matches!(
        error,
        ArtifactError::Malformed(message) if message.contains("witness_prefix_count")
    ));
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
        name: "Equatable".to_owned(),
        span: span(),
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

    let checked = program
        .clone()
        .into_checked()
        .expect("valid checked concept metadata");
    let bytes = encode_interpreted_artifact(&checked).expect("encode concept metadata");
    let decoded = decode_interpreted_artifact(&bytes).expect("decode concept metadata");
    assert_eq!(decoded.concepts.len(), 1);
    assert_eq!(decoded.requirements.len(), 1);

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
            name: "Source".to_owned(),
            span: span(),
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
    let program = program
        .into_checked()
        .expect("explicit unrefine, bindings, and Never join are valid");
    let bytes = encode_interpreted_artifact(&program).expect("encode new Core 0.1 nodes");
    decode_interpreted_artifact(&bytes).expect("round trip new Core 0.1 nodes");
}

#[test]
#[allow(clippy::too_many_lines)]
fn method_generics_have_explicit_type_arguments_and_method_proofs() {
    let concepts = vec![
        ConceptDef {
            id: ConceptId(0),
            name: "Equal".to_owned(),
            span: span(),
            dynamic: false,
            associated_types: Vec::new(),
            requirements: vec![RequirementId(0)],
        },
        ConceptDef {
            id: ConceptId(1),
            name: "Echo".to_owned(),
            span: span(),
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
        name: "Source".to_owned(),
        span: span(),
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
        name: "Source".to_owned(),
        span: span(),
        dynamic: false,
        associated_types: vec![AssociatedTypeDef {
            name: "Item".to_owned(),
            span: span(),
        }],
        requirements: vec![RequirementId(0)],
    };
    let convert = ConceptDef {
        id: ConceptId(1),
        name: "Convert".to_owned(),
        span: span(),
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
        name: "Viewable".to_owned(),
        span: span(),
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
        name: "Viewable".to_owned(),
        span: span(),
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
