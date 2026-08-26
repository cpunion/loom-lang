#![allow(clippy::default_trait_access)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use loom_codegen_ir::{SourceRoots, analyze_source_reachability};
use loom_codegen_llvm::{EmitOptions, native_object_fingerprint};
use loom_driver::AnalysisHost;
use loom_mir::{
    Block, Builtin, CallArgument, CallPlan, CallTarget, ConceptDef, ConceptId, Constant, Expr,
    ExprId, ExprKind, Function, FunctionId, LocalDecl, LocalId, Place, Program, Receiver,
    RequirementDef, RequirementId, RequirementType, Statement, StatementKind, Type, Witness,
    WitnessId, WitnessRef, decode_interpreted_artifact, encode_interpreted_artifact,
};

#[test]
fn dynamic_edges_keep_only_witnesses_constructed_by_reachable_code() {
    let mut program = Program::default();
    program.concepts.push(ConceptDef {
        id: ConceptId(0),
        name: "Display".into(),
        span: Default::default(),
        dynamic: true,
        associated_types: Vec::new(),
        requirements: vec![RequirementId(0)],
    });
    program.requirements.push(RequirementDef {
        id: RequirementId(0),
        concept: ConceptId(0),
        name: "display".into(),
        span: Default::default(),
        receiver: Some(Receiver::Readonly),
        method_type_parameters: 0,
        params: vec![RequirementType::SelfType],
        return_ty: RequirementType::Unit,
        witness_params: Vec::new(),
    });
    program.functions = vec![
        root_function(),
        unit_function(FunctionId(1), "live.display"),
        unit_function(FunctionId(2), "dead.display"),
        unit_function(FunctionId(3), "unreachable.helper"),
    ];
    program.witnesses = vec![
        Witness {
            id: WitnessId(0),
            concept: ConceptId(0),
            concrete: Type::Int,
            methods: BTreeMap::from([(RequirementId(0), FunctionId(1))]),
            associated: BTreeMap::new(),
            type_parameters: 0,
            prerequisites: Vec::new(),
        },
        Witness {
            id: WitnessId(1),
            concept: ConceptId(0),
            concrete: Type::Text,
            methods: BTreeMap::from([(RequirementId(0), FunctionId(2))]),
            associated: BTreeMap::new(),
            type_parameters: 0,
            prerequisites: Vec::new(),
        },
    ];

    let reachable = analyze_source_reachability(&program, &SourceRoots::one(FunctionId(0)))
        .expect("analyze graph");
    assert_eq!(
        reachable.functions.into_iter().collect::<Vec<_>>(),
        vec![FunctionId(0), FunctionId(1)]
    );
    assert_eq!(
        reachable.witnesses.into_iter().collect::<Vec<_>>(),
        vec![WitnessId(0)]
    );
    assert_eq!(
        reachable.witness_methods,
        BTreeMap::from([(WitnessId(0), [RequirementId(0)].into_iter().collect())])
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn dynamic_edges_use_straight_line_receiver_points_to_sets() {
    let mut program = Program::default();
    program.concepts.push(ConceptDef {
        id: ConceptId(0),
        name: "Display".into(),
        span: Default::default(),
        dynamic: true,
        associated_types: Vec::new(),
        requirements: vec![RequirementId(0)],
    });
    program.requirements.push(RequirementDef {
        id: RequirementId(0),
        concept: ConceptId(0),
        name: "display".into(),
        span: Default::default(),
        receiver: Some(Receiver::Readonly),
        method_type_parameters: 0,
        params: vec![RequirementType::SelfType],
        return_ty: RequirementType::Unit,
        witness_params: Vec::new(),
    });
    let view_ty = Type::View {
        mutable: false,
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
    };
    let live_view = make_view(
        WitnessId(0),
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Constant(Constant::Int(7)),
            ty: Type::Int,
            span: Default::default(),
        },
        view_ty.clone(),
    );
    let independently_live_view = make_view(
        WitnessId(1),
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Constant(Constant::Text("unused receiver".into())),
            ty: Type::Text,
            span: Default::default(),
        },
        view_ty.clone(),
    );
    let dynamic_call = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Call {
            target: CallTarget::Dynamic {
                requirement: RequirementId(0),
            },
            type_arguments: Vec::new(),
            arguments: vec![CallArgument::Value(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Copy(Place::local(LocalId(0))),
                ty: view_ty.clone(),
                span: Default::default(),
            })],
            witnesses: Vec::new(),
        },
        ty: Type::Unit,
        span: Default::default(),
    };
    program.functions = vec![
        Function {
            id: FunctionId(0),
            name: "main".into(),
            span: Default::default(),
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: vec![LocalDecl {
                id: LocalId(0),
                name: "display".into(),
                ty: view_ty,
                mutable: false,
                span: Default::default(),
            }],
            return_ty: Type::Unit,
            receiver: None,
            body: Block {
                statements: vec![
                    Statement {
                        kind: StatementKind::Let {
                            local: LocalId(0),
                            value: live_view,
                        },
                        span: Default::default(),
                    },
                    Statement {
                        kind: StatementKind::Evaluate(independently_live_view),
                        span: Default::default(),
                    },
                    Statement {
                        kind: StatementKind::Evaluate(dynamic_call),
                        span: Default::default(),
                    },
                ],
                tail: Some(Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Constant(Constant::Unit),
                    ty: Type::Unit,
                    span: Default::default(),
                })),
                span: Default::default(),
            },
            call_plan: CallPlan::default(),
        },
        unit_function(FunctionId(1), "int.display"),
        unit_function(FunctionId(2), "text.display"),
    ];
    program.witnesses = vec![
        Witness {
            id: WitnessId(0),
            concept: ConceptId(0),
            concrete: Type::Int,
            methods: BTreeMap::from([(RequirementId(0), FunctionId(1))]),
            associated: BTreeMap::new(),
            type_parameters: 0,
            prerequisites: Vec::new(),
        },
        Witness {
            id: WitnessId(1),
            concept: ConceptId(0),
            concrete: Type::Text,
            methods: BTreeMap::from([(RequirementId(0), FunctionId(2))]),
            associated: BTreeMap::new(),
            type_parameters: 0,
            prerequisites: Vec::new(),
        },
    ];

    let reachable = analyze_source_reachability(&program, &SourceRoots::one(FunctionId(0)))
        .expect("analyze graph");
    assert_eq!(
        reachable.functions.into_iter().collect::<Vec<_>>(),
        vec![FunctionId(0), FunctionId(1)]
    );
    assert_eq!(
        reachable.witnesses.into_iter().collect::<Vec<_>>(),
        vec![WitnessId(0), WitnessId(1)]
    );
    assert_eq!(
        reachable.witness_methods,
        BTreeMap::from([(WitnessId(0), [RequirementId(0)].into_iter().collect())])
    );
}

#[test]
fn object_fingerprint_excludes_unreachable_function_bodies() {
    let mut program = Program {
        functions: vec![
            unit_function(FunctionId(0), "main"),
            unit_function(FunctionId(1), "dead"),
        ],
        ..Program::default()
    };
    program.exports.insert("main".into(), FunctionId(0));
    let options = EmitOptions::run("main");
    let initial = native_object_fingerprint(&program, &options).expect("initial fingerprint");

    program.functions[1].name = "dead.changed".into();
    let dead_changed =
        native_object_fingerprint(&program, &options).expect("dead-body fingerprint");
    assert_eq!(initial, dead_changed);

    program.functions[0].name = "main.changed".into();
    let live_changed =
        native_object_fingerprint(&program, &options).expect("live-body fingerprint");
    assert_ne!(initial, live_changed);
}

#[test]
fn structured_builtins_scan_nested_witnesses_only_from_live_roots() {
    let view_ty = Type::View {
        mutable: false,
        concept: ConceptId(0),
        bindings: BTreeMap::new(),
    };
    let mut live = unit_function(FunctionId(0), "main");
    live.body.statements = vec![
        Statement {
            kind: StatementKind::Evaluate(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Call {
                    target: CallTarget::Builtin(Builtin::TextMapInsert),
                    type_arguments: Vec::new(),
                    arguments: vec![
                        CallArgument::Value(builtin_call(Builtin::TextMapNew, Vec::new())),
                        CallArgument::Value(Expr {
                            id: ExprId::UNASSIGNED,
                            kind: ExprKind::Constant(Constant::Text("live".into())),
                            ty: Type::Text,
                            span: Default::default(),
                        }),
                        CallArgument::Value(make_view(
                            WitnessId(0),
                            Expr {
                                id: ExprId::UNASSIGNED,
                                kind: ExprKind::Constant(Constant::Int(7)),
                                ty: Type::Int,
                                span: Default::default(),
                            },
                            view_ty.clone(),
                        )),
                    ],
                    witnesses: Vec::new(),
                },
                ty: Type::Error,
                span: Default::default(),
            }),
            span: Default::default(),
        },
        Statement {
            kind: StatementKind::Evaluate(builtin_call(
                Builtin::LogInfo,
                vec![Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Constant(Constant::Text("live".into())),
                    ty: Type::Text,
                    span: Default::default(),
                }],
            )),
            span: Default::default(),
        },
    ];
    let mut dead = unit_function(FunctionId(1), "dead");
    dead.body.statements = vec![
        Statement {
            kind: StatementKind::Evaluate(builtin_call(
                Builtin::JsonParse,
                vec![Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Constant(Constant::Text("null".into())),
                    ty: Type::Text,
                    span: Default::default(),
                }],
            )),
            span: Default::default(),
        },
        Statement {
            kind: StatementKind::Evaluate(make_view(
                WitnessId(1),
                Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Constant(Constant::Text("dead".into())),
                    ty: Type::Text,
                    span: Default::default(),
                },
                view_ty,
            )),
            span: Default::default(),
        },
    ];
    let program = Program {
        functions: vec![live, dead],
        witnesses: vec![empty_witness(WitnessId(0)), empty_witness(WitnessId(1))],
        ..Program::default()
    };

    let reachable = analyze_source_reachability(&program, &SourceRoots::one(FunctionId(0)))
        .expect("analyze graph");
    assert_eq!(reachable.functions, BTreeSet::from([FunctionId(0)]));
    assert_eq!(reachable.witnesses, BTreeSet::from([WitnessId(0)]));
    assert_eq!(
        reachable.builtins,
        BTreeSet::from([
            Builtin::TextMapNew,
            Builtin::TextMapInsert,
            Builtin::LogInfo,
        ])
    );
    assert!(!reachable.builtins.contains(&Builtin::JsonParse));
}

#[test]
fn structured_artifact_and_native_cache_identities_respect_dce_boundary() {
    let directory = tempfile::tempdir().expect("create source project");
    let source = directory.path().join("main.loom");
    fs::write(
        &source,
        r#"module cache.structured

import standard.json.parse_json
import standard.log.info
import standard.log.warn

pub fn main() Unit {
    let fields = TextMap[Text]().insert("live", "value")
    info("live")
    Unit
}

fn dead(text Text) Unit {
    let parsed = parse_json(text)
    warn("dead")
    Unit
}
"#,
    )
    .expect("write source");
    let snapshot = AnalysisHost::new(&source)
        .expect("load source")
        .snapshot()
        .expect("analyze source");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower executable MIR");
    let options = EmitOptions::run("main");
    let roots = SourceRoots::for_entry(program, "main").expect("main root");
    let reachable = analyze_source_reachability(program, &roots).expect("analyze structured graph");
    assert_eq!(
        reachable.builtins,
        BTreeSet::from([
            Builtin::TextMapNew,
            Builtin::TextMapInsert,
            Builtin::LogInfo,
        ])
    );
    assert!(!reachable.builtins.contains(&Builtin::JsonParse));
    assert!(!reachable.builtins.contains(&Builtin::LogWarn));

    let artifact = encode_interpreted_artifact(program).expect("encode structured artifact");
    assert_eq!(
        artifact,
        encode_interpreted_artifact(program).expect("encode deterministically")
    );
    let decoded = decode_interpreted_artifact(&artifact).expect("round trip structured artifact");
    assert!(decoded.prelude.text_map.is_some());
    assert!(decoded.prelude.json.is_some());
    assert!(decoded.prelude.json_error.is_some());
    assert!(decoded.prelude.io_error.is_some());
    assert!(decoded.prelude.io_error_kind.is_some());
    assert!(decoded.prelude.log_level.is_some());

    let fingerprint = native_object_fingerprint(program, &options).expect("object identity");
    let mut dead_changed = program.clone();
    replace_builtin_in_named_function(
        &mut dead_changed,
        "dead",
        Builtin::LogWarn,
        Builtin::LogError,
    );
    let dead_artifact =
        encode_interpreted_artifact(&dead_changed).expect("encode valid dead mutation");
    assert_ne!(artifact, dead_artifact);
    decode_interpreted_artifact(&dead_artifact).expect("decode valid dead mutation");
    assert_eq!(
        fingerprint,
        native_object_fingerprint(&dead_changed, &options).expect("dead-change identity")
    );

    let mut live_changed = program.clone();
    replace_builtin_in_named_function(
        &mut live_changed,
        "main",
        Builtin::LogInfo,
        Builtin::LogDebug,
    );
    let live_artifact =
        encode_interpreted_artifact(&live_changed).expect("encode valid live mutation");
    assert_ne!(artifact, live_artifact);
    decode_interpreted_artifact(&live_artifact).expect("decode valid live mutation");
    assert_ne!(
        fingerprint,
        native_object_fingerprint(&live_changed, &options).expect("live-change identity")
    );
}

fn root_function() -> Function {
    let view = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::MakeView {
            value: Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Copy(Place::local(LocalId(0))),
                ty: Type::Int,
                span: Default::default(),
            }),
            writeback: None,
            witness: WitnessRef::Concrete(WitnessId(0)),
            mutable: false,
            token: 0,
        },
        ty: Type::View {
            mutable: false,
            concept: ConceptId(0),
            bindings: BTreeMap::new(),
        },
        span: Default::default(),
    };
    let call = Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Call {
            target: CallTarget::Dynamic {
                requirement: RequirementId(0),
            },
            type_arguments: Vec::new(),
            arguments: vec![CallArgument::Value(view.clone())],
            witnesses: Vec::new(),
        },
        ty: Type::Unit,
        span: Default::default(),
    };
    let mut function = Function {
        id: FunctionId(0),
        name: "main".into(),
        span: Default::default(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![LocalDecl {
            id: LocalId(0),
            name: "value".into(),
            ty: Type::Int,
            mutable: false,
            span: Default::default(),
        }],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(view),
                    span: Default::default(),
                },
                Statement {
                    kind: StatementKind::Evaluate(call),
                    span: Default::default(),
                },
            ],
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Constant(Constant::Unit),
                ty: Type::Unit,
                span: Default::default(),
            })),
            span: Default::default(),
        },
        call_plan: CallPlan::default(),
    };
    function
        .renumber_expr_ids()
        .expect("renumber root-function expressions");
    function
}

fn make_view(witness: WitnessId, value: Expr, ty: Type) -> Expr {
    Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::MakeView {
            value: Box::new(value),
            writeback: None,
            witness: WitnessRef::Concrete(witness),
            mutable: false,
            token: 0,
        },
        ty,
        span: Default::default(),
    }
}

fn builtin_call(builtin: Builtin, arguments: Vec<Expr>) -> Expr {
    Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Call {
            target: CallTarget::Builtin(builtin),
            type_arguments: Vec::new(),
            arguments: arguments.into_iter().map(CallArgument::Value).collect(),
            witnesses: Vec::new(),
        },
        ty: Type::Error,
        span: Default::default(),
    }
}

fn empty_witness(id: WitnessId) -> Witness {
    Witness {
        id,
        concept: ConceptId(0),
        concrete: Type::Error,
        methods: BTreeMap::new(),
        associated: BTreeMap::new(),
        type_parameters: 0,
        prerequisites: Vec::new(),
    }
}

fn replace_builtin_in_named_function(
    program: &mut Program,
    function_name: &str,
    from: Builtin,
    to: Builtin,
) {
    let function = program
        .functions
        .iter_mut()
        .find(|function| function.name.rsplit('.').next() == Some(function_name))
        .unwrap_or_else(|| panic!("missing function {function_name}"));
    let mut value = serde_json::to_value(&*function).expect("serialize MIR function");
    let needle = serde_json::to_value(CallTarget::Builtin(from)).expect("serialize old builtin");
    let replacement =
        serde_json::to_value(CallTarget::Builtin(to)).expect("serialize replacement builtin");
    assert_eq!(replace_json_value(&mut value, &needle, &replacement), 1);
    *function = serde_json::from_value(value).expect("deserialize mutated MIR function");
}

fn replace_json_value(
    value: &mut serde_json::Value,
    needle: &serde_json::Value,
    replacement: &serde_json::Value,
) -> usize {
    if value == needle {
        *value = replacement.clone();
        return 1;
    }
    match value {
        serde_json::Value::Array(values) => values
            .iter_mut()
            .map(|value| replace_json_value(value, needle, replacement))
            .sum(),
        serde_json::Value::Object(fields) => fields
            .values_mut()
            .map(|value| replace_json_value(value, needle, replacement))
            .sum(),
        _ => 0,
    }
}

fn unit_function(id: FunctionId, name: &str) -> Function {
    let mut function = Function {
        id,
        name: name.into(),
        span: Default::default(),
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
            tail: Some(Box::new(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Constant(Constant::Unit),
                ty: Type::Unit,
                span: Default::default(),
            })),
            span: Default::default(),
        },
        call_plan: CallPlan::default(),
    };
    function
        .renumber_expr_ids()
        .expect("renumber unit-function expressions");
    function
}
