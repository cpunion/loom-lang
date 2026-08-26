#![allow(clippy::default_trait_access)]

use std::collections::BTreeMap;

use loom_codegen_ir::{
    GraphErrorCode, ReachableSourceGraph, SourceRoots, analyze_source_reachability,
};
use loom_mir::{
    Block, CallPlan, CallTarget, CheckedProgram, Constant, Expr, ExprId, ExprKind, Function,
    FunctionId, Program, Type,
};

fn checked(program: Program) -> CheckedProgram {
    program.into_checked().expect("valid checked-MIR fixture")
}

#[test]
fn source_roots_select_command_boundaries() {
    let program = checked(Program {
        exports: BTreeMap::from([("main".into(), FunctionId(0))]),
        tests: vec![FunctionId(1), FunctionId(2)],
        functions: vec![
            unit_function(FunctionId(0), "main", unit()),
            unit_function(FunctionId(1), "first_test", unit()),
            unit_function(FunctionId(2), "second_test", unit()),
        ],
        ..Program::default()
    });

    assert_eq!(
        SourceRoots::for_entry(&program, "main"),
        Some(SourceRoots::one(FunctionId(0)))
    );
    assert_eq!(SourceRoots::for_entry(&program, "missing"), None);
    assert_eq!(
        SourceRoots::for_tests(&program).functions(),
        &[FunctionId(1), FunctionId(2)].into_iter().collect()
    );
}

#[test]
fn direct_closure_and_serialization_are_deterministic() {
    let program = checked(Program {
        functions: vec![
            unit_function(FunctionId(0), "main", direct_call(FunctionId(1))),
            unit_function(FunctionId(1), "helper", direct_call(FunctionId(0))),
            unit_function(FunctionId(2), "dead", unit()),
        ],
        ..Program::default()
    });
    let roots = SourceRoots::one(FunctionId(0));

    let reachable = analyze_source_reachability(&program, &roots).expect("close source graph");

    assert_eq!(
        reachable,
        ReachableSourceGraph {
            functions: [FunctionId(0), FunctionId(1)].into_iter().collect(),
            ..ReachableSourceGraph::default()
        }
    );
    assert_eq!(
        serde_json::to_string(&roots).expect("serialize roots"),
        r#"{"functions":[0]}"#
    );
    assert_eq!(
        serde_json::to_string(&reachable).expect("serialize source graph"),
        r#"{"functions":[0,1],"witnesses":[],"builtins":[],"witness_methods":{}}"#
    );
}

#[test]
fn empty_test_roots_produce_an_empty_graph() {
    let program = checked(Program::default());
    let roots = SourceRoots::for_tests(&program);

    assert_eq!(
        analyze_source_reachability(&program, &roots).expect("close empty source graph"),
        ReachableSourceGraph::default()
    );
}

#[test]
fn graph_errors_have_stable_structured_codes_and_messages() {
    let program = checked(Program::default());
    let missing_function = analyze_source_reachability(&program, &SourceRoots::one(FunctionId(7)))
        .expect_err("missing root must fail");
    assert_eq!(
        missing_function.code(),
        GraphErrorCode::InvalidFunctionReference
    );
    assert_eq!(
        missing_function.message(),
        "reachable function #7 does not exist"
    );
    assert_eq!(
        missing_function.to_string(),
        "InvalidFunctionReference: reachable function #7 does not exist"
    );
}

fn unit_function(id: FunctionId, name: &str, tail: Expr) -> Function {
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
            tail: Some(Box::new(tail)),
            span: Default::default(),
        },
        call_plan: CallPlan::default(),
    };
    function
        .renumber_expr_ids()
        .expect("renumber test function expressions");
    function
}

fn unit() -> Expr {
    Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Constant(Constant::Unit),
        ty: Type::Unit,
        span: Default::default(),
    }
}

fn direct_call(target: FunctionId) -> Expr {
    Expr {
        id: ExprId::UNASSIGNED,
        kind: ExprKind::Call {
            target: CallTarget::Direct(target),
            type_arguments: Vec::new(),
            arguments: Vec::new(),
            witnesses: Vec::new(),
        },
        ty: Type::Unit,
        span: Default::default(),
    }
}
