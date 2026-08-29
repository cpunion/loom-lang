#![allow(clippy::default_trait_access)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use loom_codegen_ir::{SourceRoots, analyze_source_reachability};
use loom_codegen_llvm::{EmitOptions, native_object_fingerprint};

mod support;
use loom_mir::{
    Block, Builtin, CallArgument, CallPlan, CallTarget, CheckedProgram, ConceptDef, ConceptId,
    Constant, Expr, ExprId, ExprKind, Function, FunctionId, LocalDecl, LocalId, Place, Program,
    Receiver, RequirementDef, RequirementId, RequirementType, Statement, StatementKind, Type,
    Witness, WitnessId, WitnessRef, decode_interpreted_artifact, encode_interpreted_artifact,
};

fn checked(mut program: Program) -> CheckedProgram {
    program
        .renumber_expr_ids()
        .expect("renumber checked-MIR fixture expressions");
    program.into_checked().expect("valid checked-MIR fixture")
}

#[test]
fn dynamic_edges_keep_only_witnesses_constructed_by_reachable_code() {
    let mut program = Program::default();
    program.concepts.push(ConceptDef {
        id: ConceptId(0),
        module: "test".into(),
        name: "Display".into(),
        span: Default::default(),
        identity: None,
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
        unit_method(FunctionId(1), "live.display", Type::Int),
        unit_method(FunctionId(2), "dead.display", Type::Text),
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

    let program = checked(program);
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
        module: "test".into(),
        name: "Display".into(),
        span: Default::default(),
        identity: None,
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
        0,
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
        1,
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
        unit_method(FunctionId(1), "int.display", Type::Int),
        unit_method(FunctionId(2), "text.display", Type::Text),
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

    let program = checked(program);
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
    let program = checked(program);
    let initial = native_object_fingerprint(&program, &options).expect("initial fingerprint");

    let mut dead_changed = program.clone().into_program();
    dead_changed.functions[1].name = "dead.changed".into();
    let dead_changed = checked(dead_changed);
    let dead_fingerprint =
        native_object_fingerprint(&dead_changed, &options).expect("dead-body fingerprint");
    assert_eq!(initial, dead_fingerprint);

    let mut live_changed = dead_changed.into_program();
    live_changed.functions[0].name = "main.changed".into();
    let live_changed = checked(live_changed);
    let live_fingerprint =
        native_object_fingerprint(&live_changed, &options).expect("live-body fingerprint");
    assert_ne!(initial, live_fingerprint);
}

#[test]
fn structured_builtins_scan_nested_witnesses_only_from_live_roots() {
    let source = tempfile::tempdir().expect("create structured graph project");
    fs::write(
        source.path().join("main.loom"),
        r#"import std.json.parse_json
import std.log.info
import std.log.warn

pub dyn concept Marker {}

impl Marker for Int {}
impl Marker for Text {}

fn markInt(value Int) dyn Marker { value }
fn markText(value Text) dyn Marker { value }

pub fn main() {
    let value = markInt(7)
    let fields = TextMap[dyn Marker]().insert("live", value)
    info("live")
}

fn dead() {
    let value = markText("dead")
    let parsed = parse_json("null")
    warn("dead")
}
"#,
    )
    .expect("write structured graph source");
    let snapshot = support::analysis_host(source.path())
        .expect("load structured graph source")
        .snapshot()
        .expect("analyze structured graph source");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower structured graph MIR");
    let roots = SourceRoots::for_entry(program, "main").expect("main root");
    let reachable = analyze_source_reachability(program, &roots).expect("analyze graph");
    assert_eq!(reachable.functions.len(), 4);
    let reachable_names = program
        .functions
        .iter()
        .filter(|function| reachable.functions.contains(&function.id))
        .map(|function| function.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        reachable_names,
        BTreeSet::from([
            "std.log.info",
            "std.log.write_without_fields",
            "standalone.main",
            "standalone.markInt",
        ])
    );
    assert_eq!(reachable.witnesses.len(), 1);
    assert_eq!(
        reachable.builtins,
        BTreeSet::from([
            Builtin::TextMapNew,
            Builtin::TextMapInsert,
            Builtin::LogWrite,
        ])
    );
}

#[test]
fn structured_artifact_and_native_cache_identities_respect_dce_boundary() {
    let directory = tempfile::tempdir().expect("create source project");
    let source = directory.path().join("main.loom");
    fs::write(
        &source,
        r#"import std.json.parse_json
import std.log.info
import std.log.warn

pub fn main() {
    let fields = TextMap[Text]().insert("live", "value")
    info("live")
}

fn dead(text Text) {
    let parsed = parse_json(text)
    warn("dead")
}
"#,
    )
    .expect("write source");
    let snapshot = support::analysis_host(&source)
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
            Builtin::LogWrite,
        ])
    );
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
    let mut dead_changed = program.clone().into_program();
    replace_direct_call_in_named_function(
        &mut dead_changed,
        "dead",
        "std.log.warn",
        "std.log.error",
    );
    let dead_changed = checked(dead_changed);
    let dead_artifact =
        encode_interpreted_artifact(&dead_changed).expect("encode valid dead mutation");
    assert_ne!(artifact, dead_artifact);
    decode_interpreted_artifact(&dead_artifact).expect("decode valid dead mutation");
    assert_eq!(
        fingerprint,
        native_object_fingerprint(&dead_changed, &options).expect("dead-change identity")
    );

    let mut live_changed = program.clone().into_program();
    replace_direct_call_in_named_function(
        &mut live_changed,
        "main",
        "std.log.info",
        "std.log.debug",
    );
    let live_changed = checked(live_changed);
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
            arguments: vec![CallArgument::Value(view)],
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
            statements: vec![Statement {
                kind: StatementKind::Evaluate(call),
                span: Default::default(),
            }],
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

fn make_view(witness: WitnessId, token: u32, value: Expr, ty: Type) -> Expr {
    Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::MakeView {
            value: Box::new(value),
            writeback: None,
            witness: WitnessRef::Concrete(witness),
            mutable: false,
            token,
        },
        ty,
        span: Default::default(),
    }
}

fn replace_direct_call_in_named_function(
    program: &mut Program,
    function_name: &str,
    from: &str,
    to: &str,
) {
    let from = program
        .functions
        .iter()
        .find(|function| function.name == from)
        .unwrap_or_else(|| panic!("missing function {from}"))
        .id;
    let to = program
        .functions
        .iter()
        .find(|function| function.name == to)
        .unwrap_or_else(|| panic!("missing function {to}"))
        .id;
    let function = program
        .functions
        .iter_mut()
        .find(|function| function.name.rsplit('.').next() == Some(function_name))
        .unwrap_or_else(|| panic!("missing function {function_name}"));
    let mut value = serde_json::to_value(&*function).expect("serialize MIR function");
    let needle = serde_json::to_value(CallTarget::Direct(from)).expect("serialize old call target");
    let replacement =
        serde_json::to_value(CallTarget::Direct(to)).expect("serialize replacement call target");
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

fn unit_method(id: FunctionId, name: &str, receiver_ty: Type) -> Function {
    let mut function = unit_function(id, name);
    function.params.push(LocalDecl {
        id: LocalId(0),
        name: "self".into(),
        ty: receiver_ty,
        mutable: false,
        span: Default::default(),
    });
    function.receiver = Some(Receiver::Readonly);
    function
}
