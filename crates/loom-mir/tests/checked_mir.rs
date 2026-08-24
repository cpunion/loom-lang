use std::collections::BTreeMap;
use std::process::Command;

use loom_core::Span;
use loom_mir::{
    ArtifactError, AssociatedTypeDef, Block, CallArgument, CallPlan, CallTarget, CheckedProgram,
    ConceptDef, ConceptId, Constant, ConstructionMode, Contract, ContractArm, ContractExpr,
    ContractExprKind, ContractValue, Expr, ExprKind, FieldDef, Function, FunctionId,
    INTERPRETED_ARTIFACT_VERSION, LocalDecl, LocalId, MatchArm, MirValidationCode, Pattern, Place,
    PreludeIds, Program, Receiver, RequirementDef, RequirementId, RequirementType,
    RequirementWitnessParam, Statement, StatementKind, Type, TypeDef, TypeDefKind, TypeId,
    VariantDef, VariantId, Witness, WitnessId, WitnessParam, WitnessRef,
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

fn constant(value: Constant, ty: Type) -> Expr {
    Expr {
        kind: ExprKind::Constant(value),
        ty,
        span: span(),
    }
}

fn copy(id: u32, ty: Type) -> Expr {
    Expr {
        kind: ExprKind::Copy(Place::local(LocalId(id))),
        ty,
        span: span(),
    }
}

fn function(
    id: u32,
    params: Vec<LocalDecl>,
    locals: Vec<LocalDecl>,
    return_ty: Type,
    body: Block,
) -> Function {
    Function {
        id: FunctionId(id),
        name: format!("function_{id}"),
        span: span(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params,
        witness_params: Vec::new(),
        locals,
        return_ty,
        receiver: None,
        body,
        call_plan: CallPlan::default(),
    }
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

#[test]
fn valid_program_crosses_checked_boundary() {
    let program = simple_program();
    validate_program(&program).expect("valid MIR");
    let checked = CheckedProgram::new(program).expect("checked MIR");
    assert_eq!(checked.functions.len(), 1);
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
            kind: ExprKind::Refine {
                ty: money,
                value: Box::new(constant(Constant::Float(1.0), Type::Float)),
                construction: ConstructionMode::Proven,
            },
            ty: Type::Nominal(money, Vec::new()),
            span: span(),
        },
        Expr {
            kind: ExprKind::Refine {
                ty: money,
                value: Box::new(constant(Constant::Float(1.0), Type::Float)),
                construction: ConstructionMode::Runtime,
            },
            ty: result_of(money),
            span: span(),
        },
        Expr {
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
        },
        ..Program::default()
    };
    let errors = validation_errors(&program);
    assert!(errors.contains(MirValidationCode::InvalidTypeReference));
    assert!(errors.contains(MirValidationCode::RecordShape));
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

fn float_program(bits: u64) -> Program {
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
        encode_interpreted_artifact(decoded.as_program()).expect("re-encode"),
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

    let bytes = encode_interpreted_artifact(&program).expect("encode concept metadata");
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
    validate_program(&program).expect("explicit unrefine, bindings, and Never join are valid");
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
    let artifact = encode_interpreted_artifact(&program).expect_err("artifact must fail closed");
    assert!(matches!(
        artifact,
        ArtifactError::InvalidProgram(ref errors)
            if errors.contains(MirValidationCode::NestingLimit)
    ));
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

#[test]
fn artifact_rejects_wire_nesting_before_recursive_decode() {
    let mut hostile = vec![b'['; 20_000];
    hostile.extend(std::iter::repeat_n(b']', 20_000));
    assert!(matches!(
        decode_interpreted_artifact(&hostile),
        Err(ArtifactError::Malformed(_))
    ));
}
