#![allow(clippy::default_trait_access)]

use std::collections::BTreeMap;

use loom_codegen_llvm::{EmitOptions, Roots, analyze_reachability, native_object_fingerprint};
use loom_mir::{
    Block, CallArgument, CallPlan, CallTarget, ConceptDef, ConceptId, Constant, Expr, ExprKind,
    Function, FunctionId, LocalDecl, LocalId, Place, Program, Receiver, RequirementDef,
    RequirementId, RequirementType, Statement, StatementKind, Type, Witness, WitnessId, WitnessRef,
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

    let reachable =
        analyze_reachability(&program, &Roots::one(FunctionId(0))).expect("analyze graph");
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

fn root_function() -> Function {
    let view = Expr {
        kind: ExprKind::MakeView {
            value: Box::new(Expr {
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
    Function {
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
                kind: ExprKind::Constant(Constant::Unit),
                ty: Type::Unit,
                span: Default::default(),
            })),
            span: Default::default(),
        },
        call_plan: CallPlan::default(),
    }
}

fn unit_function(id: FunctionId, name: &str) -> Function {
    Function {
        id,
        name: name.into(),
        span: Default::default(),
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
            tail: Some(Box::new(Expr {
                kind: ExprKind::Constant(Constant::Unit),
                ty: Type::Unit,
                span: Default::default(),
            })),
            span: Default::default(),
        },
        call_plan: CallPlan::default(),
    }
}
