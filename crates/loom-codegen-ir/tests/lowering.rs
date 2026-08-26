use loom_codegen_ir::{
    InstanceKey, InstructionKind, InvalidRootCode, LoweringErrorCode, LoweringOutcome,
    SourceArtifactRequest, TargetLayout, UnsupportedFeature, dump_program, lower_typed_artifact,
};
use loom_core::FileId;
use loom_hir::{SourceUnit, lower_files};
use loom_lowering::lower_to_mir;
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fn compile(source: &str) -> loom_mir::CheckedProgram {
    let parsed = parse_with_file(FileId(0), source);
    assert!(
        parsed.diagnostics().is_empty(),
        "syntax diagnostics: {:#?}",
        parsed.diagnostics()
    );
    let lowered = lower_files([SourceUnit {
        file: FileId(0),
        syntax: parsed.ast(),
    }]);
    assert!(
        lowered.diagnostics.is_empty(),
        "HIR diagnostics: {:#?}",
        lowered.diagnostics
    );
    let analysis = analyze(&lowered.program);
    assert!(
        analysis.diagnostics.is_empty(),
        "semantic diagnostics: {:#?}",
        analysis.diagnostics
    );
    lower_to_mir(&lowered.program, &analysis)
        .unwrap_or_else(|failure| panic!("MIR lowering diagnostics: {:#?}", failure.diagnostics()))
}

fn lower_run(source: &str) -> LoweringOutcome {
    let mir = compile(source);
    lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower typed artifact")
}

fn complete_dump(source: &str) -> String {
    let LoweringOutcome::Complete(artifact) = lower_run(source) else {
        panic!("source should be completely supported")
    };
    dump_program(artifact.program())
}

#[test]
fn empty_tests_are_one_complete_empty_artifact() {
    let mir = compile("module empty\n");
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower empty tests");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("empty test artifact should be complete")
    };
    assert!(artifact.functions().is_empty());
    assert_eq!(artifact.test_roots(), Some([].as_slice()));
}

#[test]
fn ordered_test_roots_form_one_complete_artifact() {
    let mir =
        compile("module tests\n\ntest fn first() { Unit }\n\ntest fn second() Unit { Unit }\n");
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower test artifact");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("scalar tests should be complete")
    };
    let roots = artifact.test_roots().expect("test roots");
    assert_eq!(roots.len(), 2);
    assert_eq!(
        roots
            .iter()
            .map(|root| artifact.function(*root).expect("root function").name())
            .collect::<Vec<_>>(),
        ["tests.first", "tests.second"]
    );
}

#[test]
fn source_lowering_routes_declarations_calls_and_roots_through_monomorphic_instance_keys() {
    let LoweringOutcome::Complete(artifact) = lower_run(
        r"module instance_regression

fn helper() Unit { Unit }

pub fn main() Unit { helper() }
",
    ) else {
        panic!("scalar source should lower completely")
    };
    let program = artifact.program().as_program();
    assert_eq!(program.instances().entries().len(), 2);
    for instance in program.instances().entries() {
        assert!(instance.key().is_monomorphic());
        assert_eq!(program.instance_key(instance.id()), Some(instance.key()));
        assert_eq!(
            program.instances().find(instance.key()),
            Some(instance.id())
        );
        assert_eq!(
            program
                .function(instance.id())
                .expect("planned function")
                .source(),
            instance.key().source()
        );
    }

    let root = artifact.run_root().expect("run root");
    let root_function = program.function(root).expect("root function");
    assert_eq!(
        program.instance_key(root),
        Some(&InstanceKey::monomorphic(root_function.source()))
    );
    let callees = program
        .functions()
        .iter()
        .flat_map(loom_codegen_ir::Function::instructions)
        .filter_map(|instruction| match instruction.kind() {
            InstructionKind::DirectCall { callee, .. } => Some(*callee),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(callees.len(), 1);
    let callee = callees[0];
    let callee_function = program.function(callee).expect("call target");
    assert_eq!(
        program.instance_key(callee),
        Some(&InstanceKey::monomorphic(callee_function.source()))
    );
}

#[test]
fn callable_instance_foundation_does_not_enable_generic_lowering() {
    let outcome = lower_run(
        r"module generic_fallback

fn identity[T](value T) T { value }

pub fn main() Unit {
    discard identity(1)
    Unit
}
",
    );
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("generic source must still select whole-artifact fallback")
    };
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::GenericFunction)
    );
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::GenericCall)
    );
}

#[test]
fn sema_valid_fallible_test_root_is_unsupported_not_invalid() {
    let mir = compile(
        r"module fallible_tests

enum Problem { Failed }

test fn fallible() Result[Unit, Problem] { Ok(Unit) }
",
    );
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect("a sema-valid test signature must reach coverage classification");
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("Result-returning test is outside the typed slice")
    };
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::SignatureType)
    );
}

#[test]
fn sema_invalid_test_return_is_an_invalid_root_not_fallback() {
    use loom_mir::{
        Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, Program, Type,
    };

    let span = loom_core::Span::default();
    let mut invalid_test = Function {
        id: FunctionId(0),
        name: "manual.invalid_test".into(),
        span,
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
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Bool(true)),
                Type::Bool,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    invalid_test
        .renumber_expr_ids()
        .expect("number invalid test root");
    let mir = Program {
        functions: vec![invalid_test],
        tests: vec![FunctionId(0)],
        ..Program::default()
    }
    .into_checked()
    .expect("checked MIR permits the command boundary to validate test returns");

    let error = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("test target"),
    )
    .expect_err("a Bool-returning test cannot select fallback");
    assert_eq!(
        error.code(),
        LoweringErrorCode::InvalidRoot(InvalidRootCode::RootSignature)
    );
}

#[test]
fn hidden_run_root_inputs_are_invalid_not_unsupported() {
    use loom_mir::{
        Block, CallPlan, ConceptDef, ConceptId, Constant, Expr, ExprKind, Function, FunctionId,
        LocalDecl, LocalId, Program, Receiver, Type, WitnessParam,
    };

    let span = loom_core::Span::default();
    let unit_root = || Function {
        id: FunctionId(0),
        name: "manual.main".into(),
        span,
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
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    let assert_invalid = |mut function: Function, concepts: Vec<ConceptDef>| {
        function
            .renumber_expr_ids()
            .expect("number invalid run root");
        let mir = Program {
            functions: vec![function],
            concepts,
            exports: std::collections::BTreeMap::from([("main".into(), FunctionId(0))]),
            ..Program::default()
        }
        .into_checked()
        .expect("hidden run inputs are valid inside checked MIR");
        let error = lower_typed_artifact(
            &mir,
            &SourceArtifactRequest::Run {
                entry: "main".into(),
            },
            TargetLayout::new(64).expect("test target"),
        )
        .expect_err("a run harness cannot supply hidden inputs");
        assert_eq!(
            error.code(),
            LoweringErrorCode::InvalidRoot(InvalidRootCode::RootSignature)
        );
    };

    let mut generic = unit_root();
    generic.type_parameters = 1;
    assert_invalid(generic, Vec::new());

    let mut witnessed = unit_root();
    witnessed.witness_params.push(WitnessParam {
        target: Type::Int,
        concept: ConceptId(0),
        bindings: std::collections::BTreeMap::new(),
        span,
    });
    assert_invalid(
        witnessed,
        vec![ConceptDef {
            id: ConceptId(0),
            name: "Marker".into(),
            span,
            dynamic: true,
            associated_types: Vec::new(),
            requirements: Vec::new(),
        }],
    );

    let mut inherent = unit_root();
    inherent.params.push(LocalDecl {
        id: LocalId(0),
        name: "self".into(),
        ty: Type::Int,
        mutable: false,
        span,
    });
    inherent.receiver = Some(Receiver::Readonly);
    assert_invalid(inherent, Vec::new());
}

#[test]
fn invalid_run_name_is_an_error_not_unsupported() {
    let mir = compile("module roots\n\npub fn main() Unit { Unit }\n");
    let error = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "missing".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect_err("unknown entry must fail");
    assert_eq!(
        error.code(),
        LoweringErrorCode::InvalidRoot(InvalidRootCode::UnknownEntry)
    );
}

#[test]
fn unsupported_unreachable_functions_do_not_select_fallback() {
    let dump = complete_dump(
        r#"module unreachable

fn deadText() Text { "legacy" }

pub fn main() Unit { Unit }
"#,
    );
    assert!(dump.contains("fn i0 mir=f1 \"unreachable.main\""), "{dump}");
    assert!(!dump.contains("deadText"), "{dump}");
}

#[test]
fn unreachable_code_inside_a_reachable_function_is_ignored_exactly() {
    let outcome = lower_run(
        r#"module dead_control

fn helper() Unit { Unit }

pub fn main() Unit {
    return Unit
    let legacy = "legacy"
    discard legacy
    helper()
    Unit
}
"#,
    );
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("dead unsupported values and calls must not select fallback")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.contains("dead_control.main"), "{dump}");
    assert!(!dump.contains("dead_control.helper"), "{dump}");
    assert!(!dump.contains("text"), "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn diverging_prefixes_do_not_require_unmaterialized_unsupported_heads() {
    use loom_mir::{
        Block, CallArgument, CallPlan, CallTarget, Constant, Expr, ExprKind, Function, FunctionId,
        LocalDecl, LocalId, Program, Statement, StatementKind, Type,
    };

    let span = loom_core::Span::default();
    let never_return = || {
        Expr::new(
            ExprKind::Block(Block {
                statements: vec![Statement {
                    kind: StatementKind::Return(Some(Expr::new(
                        ExprKind::Constant(Constant::Unit),
                        Type::Unit,
                        span,
                    ))),
                    span,
                }],
                tail: None,
                span,
            }),
            Type::Never,
            span,
        )
    };
    let root = |id: u32, name: &str, statement: Statement| {
        let mut function = Function {
            id: FunctionId(id),
            name: name.into(),
            span,
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
                statements: vec![statement],
                tail: Some(Box::new(Expr::new(
                    ExprKind::Constant(Constant::Unit),
                    Type::Unit,
                    span,
                ))),
                span,
            },
            call_plan: CallPlan::default(),
        };
        function.renumber_expr_ids().expect("number dead head");
        function
    };

    let call = root(
        0,
        "manual.dead_call_head",
        Statement {
            kind: StatementKind::Evaluate(Expr::new(
                ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(5)),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(never_return())],
                    witnesses: Vec::new(),
                },
                Type::Text,
                span,
            )),
            span,
        },
    );
    let tuple = root(
        1,
        "manual.dead_tuple_head",
        Statement {
            kind: StatementKind::Evaluate(Expr::new(
                ExprKind::Tuple(vec![
                    never_return(),
                    Expr::new(
                        ExprKind::Constant(Constant::Text("dead".into())),
                        Type::Text,
                        span,
                    ),
                ]),
                Type::Tuple(vec![Type::Never, Type::Text]),
                span,
            )),
            span,
        },
    );
    let list = root(
        2,
        "manual.dead_list_head",
        Statement {
            kind: StatementKind::Evaluate(Expr::new(
                ExprKind::List(vec![
                    never_return(),
                    Expr::new(
                        ExprKind::Constant(Constant::Text("dead".into())),
                        Type::Text,
                        span,
                    ),
                ]),
                Type::List(Box::new(Type::Text)),
                span,
            )),
            span,
        },
    );
    let returning_branch = || Block {
        statements: vec![Statement {
            kind: StatementKind::Return(Some(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        }],
        tail: None,
        span,
    };
    let conditional = root(
        3,
        "manual.dead_if_result",
        Statement {
            kind: StatementKind::Evaluate(Expr::new(
                ExprKind::If {
                    condition: Box::new(Expr::new(
                        ExprKind::Constant(Constant::Bool(true)),
                        Type::Bool,
                        span,
                    )),
                    then_branch: returning_branch(),
                    else_branch: returning_branch(),
                },
                Type::Text,
                span,
            )),
            span,
        },
    );
    let assertion = root(
        4,
        "manual.dead_assert_head",
        Statement {
            kind: StatementKind::Assert {
                condition: never_return(),
            },
            span,
        },
    );
    let mut dead_target = Function {
        id: FunctionId(5),
        name: "manual.dead_text_target".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![LocalDecl {
            id: LocalId(0),
            name: "value".into(),
            ty: Type::Unit,
            mutable: false,
            span,
        }],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Text,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Text("dead".into())),
                Type::Text,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    dead_target.renumber_expr_ids().expect("number dead target");
    let mir = Program {
        functions: vec![call, tuple, list, conditional, assertion, dead_target],
        tests: (0..5).map(FunctionId).collect(),
        ..Program::default()
    }
    .into_checked()
    .expect("checked dead-head MIR");

    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Tests,
        TargetLayout::new(64).expect("target"),
    )
    .expect("lower dead heads") else {
        panic!("unmaterialized unsupported heads must not select fallback")
    };
    let dump = dump_program(artifact.program());
    assert_eq!(artifact.functions().len(), 5, "{dump}");
    assert!(!dump.contains("dead_text_target"), "{dump}");
    assert!(!dump.contains("const text"), "{dump}");
}

#[test]
fn reachable_unsupported_sites_report_the_whole_artifact_before_building() {
    let mir = compile(
        r#"module coverage

fn textValue() Text { "legacy" }

pub fn main() Unit {
    let value = textValue()
    Unit
}
"#,
    );
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("classification succeeds");
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("reachable Text must select whole-artifact fallback")
    };
    let repeated = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("repeat classification");
    let LoweringOutcome::Unsupported(repeated) = repeated else {
        panic!("repeated reachable Text classification must be unsupported")
    };
    assert_eq!(report, repeated);
    assert!(
        report
            .items()
            .iter()
            .any(|item| item.feature() == UnsupportedFeature::SignatureType)
    );
    assert!(
        report.items().iter().all(|item| !item.path().is_empty())
            && report
                .items()
                .iter()
                .any(|item| item.expression().is_some())
    );
}

#[test]
fn scalar_constants_locals_blocks_short_circuit_and_returns_dump_as_typed_ssa() {
    let dump = complete_dump(
        r"module scalar

fn choose(flag Bool, integer Int, decimal Float) Int {
    var selected = 0
    if flag && decimal != 0.0 {
        selected = integer
        Unit
    } else {
        selected = 7
        Unit
    }
    if flag {
        return selected
    } else {
        selected + 1
    }
}

pub fn main() Unit {
    let output = choose(true, 41, -1.5)
    discard output == 41
    Unit
}
",
    );
    for expected in [
        "effects=may_fault",
        "const bool true",
        "const int 41",
        "const float 0x3ff8000000000000",
        "float.compare.unordered_not_equal",
        "float.negate",
        "branch",
        "checked_int.add",
        "invoke i0",
        "int.compare.equal",
    ] {
        assert!(dump.contains(expected), "missing `{expected}`:\n{dump}");
    }
}

#[test]
fn implicit_unit_and_all_explicit_return_branches_are_supported() {
    let dump = complete_dump(
        r"module returns

fn implicitUnit() {}

fn selected(flag Bool) Int {
    if flag {
        return 1
    } else {
        return 2
    }
}

pub fn main() Unit {
    implicitUnit()
    let value = selected(false)
    Unit
}
",
    );
    assert!(dump.contains("\"returns.implicitUnit\""), "{dump}");
    assert!(dump.matches("return").count() >= 4, "{dump}");
    assert!(dump.contains("call i0()"), "{dump}");
}

#[test]
fn pure_recursive_cycle_stays_infallible_and_uses_direct_calls() {
    let dump = complete_dump(
        r"module pure_recursion

fn recurse(flag Bool) Unit {
    if flag {
        recurse(flag)
    } else {
        Unit
    }
}

pub fn main() Unit {
    recurse(false)
}
",
    );
    assert_eq!(dump.matches("effects=none").count(), 2, "{dump}");
    assert!(dump.matches("call i0(").count() >= 2, "{dump}");
    assert!(!dump.contains("invoke"), "{dump}");
    assert!(!dump.contains("resume_fault"), "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn arithmetic_after_a_diverging_operand_does_not_seed_fault_effects() {
    use loom_mir::{
        BinaryOp, Block, CallPlan, CallTarget, Constant, Expr, ExprKind, Function, FunctionId,
        Program, Statement, StatementKind, Type,
    };

    let span = loom_core::Span::default();
    let mut stops = Function {
        id: FunctionId(0),
        name: "manual.stops_before_add".into(),
        span,
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
            tail: Some(Box::new(Expr::new(
                ExprKind::Binary(
                    BinaryOp::Add,
                    Box::new(Expr::new(
                        ExprKind::Constant(Constant::Int(1)),
                        Type::Int,
                        span,
                    )),
                    Box::new(Expr::new(
                        ExprKind::Block(Block {
                            statements: vec![Statement {
                                kind: StatementKind::Return(Some(Expr::new(
                                    ExprKind::Constant(Constant::Int(7)),
                                    Type::Int,
                                    span,
                                ))),
                                span,
                            }],
                            tail: None,
                            span,
                        }),
                        Type::Never,
                        span,
                    )),
                ),
                Type::Int,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    let mut main = Function {
        id: FunctionId(1),
        name: "manual.main".into(),
        span,
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
            statements: vec![Statement {
                kind: StatementKind::Evaluate(Expr::new(
                    ExprKind::Call {
                        target: CallTarget::Direct(FunctionId(0)),
                        type_arguments: Vec::new(),
                        arguments: Vec::new(),
                        witnesses: Vec::new(),
                    },
                    Type::Int,
                    span,
                )),
                span,
            }],
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    stops.renumber_expr_ids().expect("number diverging add");
    main.renumber_expr_ids().expect("number caller");
    let mir = Program {
        exports: BTreeMap::from([("main".into(), FunctionId(1))]),
        functions: vec![stops, main],
        ..Program::default()
    }
    .into_checked()
    .expect("checked diverging arithmetic MIR");
    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("target"),
    )
    .expect("lower diverging arithmetic") else {
        panic!("diverging arithmetic prefix is scalar-complete")
    };
    let dump = dump_program(artifact.program());
    assert_eq!(dump.matches("effects=none").count(), 2, "{dump}");
    assert!(dump.contains("call i0()"), "{dump}");
    assert!(!dump.contains("checked_int.add"), "{dump}");
    assert!(!dump.contains("invoke"), "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn checked_mir_move_reassignment_and_readonly_inherent_scalar_call_are_supported() {
    use loom_mir::{
        BinaryOp, Block, CallArgument, CallPlan, CallTarget, Constant, Expr, ExprKind, Function,
        FunctionId, LocalDecl, LocalId, Place, Program, Receiver, Statement, StatementKind, Type,
    };

    let span = loom_core::Span::default();
    let mut same = Function {
        id: FunctionId(0),
        name: "manual.same".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: vec![
            LocalDecl {
                id: LocalId(0),
                name: "self".into(),
                ty: Type::Bool,
                mutable: false,
                span,
            },
            LocalDecl {
                id: LocalId(1),
                name: "other".into(),
                ty: Type::Bool,
                mutable: false,
                span,
            },
        ],
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Bool,
        receiver: Some(Receiver::Readonly),
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr::new(
                ExprKind::Binary(
                    BinaryOp::Equal,
                    Box::new(Expr::new(
                        ExprKind::Copy(Place::local(LocalId(0))),
                        Type::Bool,
                        span,
                    )),
                    Box::new(Expr::new(
                        ExprKind::Copy(Place::local(LocalId(1))),
                        Type::Bool,
                        span,
                    )),
                ),
                Type::Bool,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    let mut main = Function {
        id: FunctionId(1),
        name: "manual.main".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![
            LocalDecl {
                id: LocalId(0),
                name: "moved".into(),
                ty: Type::Int,
                mutable: true,
                span,
            },
            LocalDecl {
                id: LocalId(1),
                name: "saved".into(),
                ty: Type::Int,
                mutable: false,
                span,
            },
        ],
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: Expr::new(ExprKind::Constant(Constant::Int(1)), Type::Int, span),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(1),
                        value: Expr::new(ExprKind::Move(Place::local(LocalId(0))), Type::Int, span),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(LocalId(0)),
                        value: Expr::new(ExprKind::Constant(Constant::Int(2)), Type::Int, span),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr::new(
                        ExprKind::Copy(Place::local(LocalId(1))),
                        Type::Int,
                        span,
                    )),
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr::new(
                        ExprKind::Call {
                            target: CallTarget::Inherent(FunctionId(0)),
                            type_arguments: Vec::new(),
                            arguments: vec![
                                CallArgument::Value(Expr::new(
                                    ExprKind::Constant(Constant::Bool(true)),
                                    Type::Bool,
                                    span,
                                )),
                                CallArgument::Value(Expr::new(
                                    ExprKind::Constant(Constant::Bool(false)),
                                    Type::Bool,
                                    span,
                                )),
                            ],
                            witnesses: Vec::new(),
                        },
                        Type::Bool,
                        span,
                    )),
                    span,
                },
            ],
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                span,
            ))),
            span,
        },
        call_plan: CallPlan::default(),
    };
    same.renumber_expr_ids().expect("number inherent body");
    main.renumber_expr_ids().expect("number root body");
    let mir = Program {
        exports: BTreeMap::from([("main".into(), FunctionId(1))]),
        functions: vec![same, main],
        ..Program::default()
    }
    .into_checked()
    .expect("checked manual scalar MIR");

    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower manual scalar MIR");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("manual scalar MIR should be supported")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.contains("bool.compare.equal"), "{dump}");
    assert!(dump.contains("call i0("), "{dump}");
    assert_eq!(dump.matches("effects=none").count(), 2, "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn conditional_moves_preserve_only_values_available_on_continuing_paths() {
    use loom_mir::{
        Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, LocalDecl, LocalId, Place,
        Program, Statement, StatementKind, Type,
    };

    let span = loom_core::Span::default();
    let copy = |local, ty| Expr::new(ExprKind::Copy(Place::local(local)), ty, span);
    let moved = |local| Expr::new(ExprKind::Move(Place::local(local)), Type::Int, span);
    let unit = || Expr::new(ExprKind::Constant(Constant::Unit), Type::Unit, span);
    let empty_unit_block = || Block {
        statements: Vec::new(),
        tail: Some(Box::new(unit())),
        span,
    };
    let flag = LocalId(0);
    let preserved = LocalId(1);
    let intersected = LocalId(2);

    let move_then_return = Block {
        statements: vec![
            Statement {
                kind: StatementKind::Evaluate(moved(preserved)),
                span,
            },
            Statement {
                kind: StatementKind::Return(Some(unit())),
                span,
            },
        ],
        tail: None,
        span,
    };
    let move_then_continue = Block {
        statements: vec![Statement {
            kind: StatementKind::Evaluate(moved(intersected)),
            span,
        }],
        tail: Some(Box::new(copy(flag, Type::Bool))),
        span,
    };
    let continue_without_move = Block {
        statements: Vec::new(),
        tail: Some(Box::new(copy(flag, Type::Bool))),
        span,
    };
    let mut main = Function {
        id: FunctionId(0),
        name: "manual.main".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![
            LocalDecl {
                id: flag,
                name: "flag".into(),
                ty: Type::Bool,
                mutable: false,
                span,
            },
            LocalDecl {
                id: preserved,
                name: "preserved".into(),
                ty: Type::Int,
                mutable: false,
                span,
            },
            LocalDecl {
                id: intersected,
                name: "intersected".into(),
                ty: Type::Int,
                mutable: false,
                span,
            },
        ],
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Let {
                        local: flag,
                        value: Expr::new(
                            ExprKind::Constant(Constant::Bool(true)),
                            Type::Bool,
                            span,
                        ),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Let {
                        local: preserved,
                        value: Expr::new(ExprKind::Constant(Constant::Int(11)), Type::Int, span),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Let {
                        local: intersected,
                        value: Expr::new(ExprKind::Constant(Constant::Int(22)), Type::Int, span),
                    },
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr::new(
                        ExprKind::If {
                            condition: Box::new(copy(flag, Type::Bool)),
                            then_branch: move_then_return,
                            else_branch: empty_unit_block(),
                        },
                        Type::Unit,
                        span,
                    )),
                    span,
                },
                // The move occurred only on the terminated arm, so the sole
                // continuing environment must still contain this local.
                Statement {
                    kind: StatementKind::Evaluate(copy(preserved, Type::Int)),
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(Expr::new(
                        ExprKind::If {
                            condition: Box::new(copy(flag, Type::Bool)),
                            then_branch: move_then_continue,
                            else_branch: continue_without_move,
                        },
                        Type::Bool,
                        span,
                    )),
                    span,
                },
            ],
            tail: Some(Box::new(unit())),
            span,
        },
        call_plan: CallPlan::default(),
    };
    main.renumber_expr_ids().expect("number conditional moves");
    let mir = Program {
        exports: BTreeMap::from([("main".into(), FunctionId(0))]),
        functions: vec![main],
        ..Program::default()
    }
    .into_checked()
    .expect("conditional moves satisfy checked-MIR continuation rules");

    let LoweringOutcome::Complete(artifact) = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower conditional moves") else {
        panic!("conditional scalar moves should be supported")
    };
    let dump = dump_program(artifact.program());
    assert_eq!(dump.matches("branch ").count(), 2, "{dump}");
    let jumps = dump
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("jump "))
        .collect::<Vec<_>>();
    assert_eq!(jumps.len(), 2, "{dump}");
    assert!(jumps.iter().all(|jump| jump.ends_with("()")), "{dump}");
}

#[test]
fn source_nested_blocks_and_if_arms_preserve_function_local_values() {
    let dump = complete_dump(
        r"module local_flow

fn throughBlock() Int {
    var value = 0
    {
        value = 41
        Unit
    }
    value
}

fn throughBranches(flag Bool) Int {
    var value = 0
    if flag {
        value = 1
        Unit
    } else {
        value = 2
        Unit
    }
    value
}

pub fn main() Unit {
    discard throughBlock()
    discard throughBranches(true)
    Unit
}
",
    );
    assert!(dump.contains("local_flow.throughBlock"), "{dump}");
    assert!(dump.contains("local_flow.throughBranches"), "{dump}");
    assert!(dump.contains("branch"), "{dump}");
}

#[test]
fn canonical_joins_omit_single_path_and_identity_parameters() {
    let dump = complete_dump(
        r"module canonical_join

fn onePath(flag Bool) Int {
    if flag {
        return 1
    } else {
        2
    }
}

fn sameValue(flag Bool, value Int) Int {
    if flag { value } else { value }
}

pub fn main() Unit {
    discard onePath(false)
    discard sameValue(true, 7)
    Unit
}
",
    );
    let one_path = dump
        .split("fn i1")
        .next()
        .expect("onePath function section");
    assert!(!one_path.contains("jump"), "{one_path}");

    let same_value = dump
        .split("fn i1")
        .nth(1)
        .and_then(|rest| rest.split("fn i2").next())
        .expect("sameValue function section");
    assert_eq!(same_value.matches("jump b3()").count(), 2, "{same_value}");
    assert!(same_value.contains("\n  b3:\n"), "{same_value}");
}

#[test]
fn short_circuit_skip_edge_reuses_the_lhs_without_a_constant_block() {
    let dump = complete_dump(
        r"module canonical_short_circuit

fn stopOnRhs(flag Bool) Bool {
    flag && { return flag }
}

pub fn main() Unit {
    discard stopOnRhs(false)
    Unit
}
",
    );
    let function = dump
        .split("fn i1")
        .next()
        .expect("short-circuit function section");
    assert!(function.contains("branch %v0, b1(), b2()"), "{function}");
    assert!(!function.contains("const bool"), "{function}");
    assert!(!function.contains("jump"), "{function}");
    assert_eq!(function.matches("\n  b").count(), 3, "{function}");
}

#[test]
fn range_header_carries_only_values_changed_on_continuing_paths() {
    let dump = complete_dump(
        r"module canonical_range

fn accumulate(limit Int, readonly Int) Int {
    var changed = 0
    for index in 0..limit {
        changed = changed + readonly
        Unit
    }
    changed
}

pub fn main() Unit {
    discard accumulate(3, 2)
    Unit
}
",
    );
    let function = dump.split("fn i1").next().expect("range function section");
    let header = function
        .lines()
        .find(|line| line.trim_start().starts_with("b1("))
        .expect("range header");
    assert_eq!(header.matches(": t3").count(), 2, "{function}");
    let jumps = function
        .lines()
        .filter(|line| line.contains("jump b1("))
        .collect::<Vec<_>>();
    assert_eq!(jumps.len(), 2, "{function}");
    for jump in jumps {
        assert_eq!(jump.matches('%').count(), 2, "{function}");
    }
    assert!(function.contains("int.successor_below"), "{function}");
}

#[test]
fn pure_and_nested_ranges_use_proved_successors_without_fault_effects() {
    let dump = complete_dump(
        r"module pure_ranges

fn lastBelow(limit Int) Int {
    var last = 0
    for index in 0..limit {
        last = index
        Unit
    }
    last
}

fn nested(outer Int, inner Int) Int {
    var last = 0
    for first in 0..outer {
        for second in 0..inner {
            last = second
            Unit
        }
        last = first
        Unit
    }
    last
}

pub fn main() Unit {
    discard lastBelow(8)
    discard nested(3, 4)
    Unit
}
",
    );

    assert_eq!(dump.matches("effects=none").count(), 3, "{dump}");
    assert_eq!(dump.matches("int.successor_below").count(), 3, "{dump}");
    assert!(!dump.contains("checked_int.add"), "{dump}");
    assert!(!dump.contains("invoke"), "{dump}");
    assert!(!dump.contains("resume_fault"), "{dump}");
    assert_eq!(dump.matches("call i").count(), 2, "{dump}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn checked_mir_locals_initialized_in_a_block_or_both_if_arms_survive() {
    use loom_mir::{
        Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, LocalDecl, LocalId, Place,
        Program, Statement, StatementKind, Type,
    };

    let span = loom_core::Span::default();
    let unit = || Expr::new(ExprKind::Constant(Constant::Unit), Type::Unit, span);
    let integer = |value| Expr::new(ExprKind::Constant(Constant::Int(value)), Type::Int, span);
    let copy_int = |local| Expr::new(ExprKind::Copy(Place::local(local)), Type::Int, span);
    let let_integer = |local, value| Statement {
        kind: StatementKind::Let {
            local,
            value: integer(value),
        },
        span,
    };

    let block_local = LocalId(0);
    let branch_local = LocalId(1);
    let nested = Expr::new(
        ExprKind::Block(Block {
            statements: vec![let_integer(block_local, 7)],
            tail: Some(Box::new(unit())),
            span,
        }),
        Type::Unit,
        span,
    );
    let branch = Expr::new(
        ExprKind::If {
            condition: Box::new(Expr::new(
                ExprKind::Constant(Constant::Bool(true)),
                Type::Bool,
                span,
            )),
            then_branch: Block {
                statements: vec![let_integer(branch_local, 11)],
                tail: Some(Box::new(unit())),
                span,
            },
            else_branch: Block {
                statements: vec![let_integer(branch_local, 13)],
                tail: Some(Box::new(unit())),
                span,
            },
        },
        Type::Unit,
        span,
    );
    let mut function = Function {
        id: FunctionId(0),
        name: "manual.main".into(),
        span,
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: vec![
            LocalDecl {
                id: block_local,
                name: "from_block".into(),
                ty: Type::Int,
                mutable: false,
                span,
            },
            LocalDecl {
                id: branch_local,
                name: "from_branches".into(),
                ty: Type::Int,
                mutable: false,
                span,
            },
        ],
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(nested),
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(copy_int(block_local)),
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(branch),
                    span,
                },
                Statement {
                    kind: StatementKind::Evaluate(copy_int(branch_local)),
                    span,
                },
            ],
            tail: Some(Box::new(unit())),
            span,
        },
        call_plan: CallPlan::default(),
    };
    function.renumber_expr_ids().expect("number local-flow MIR");
    let mir = Program {
        exports: BTreeMap::from([("main".into(), FunctionId(0))]),
        functions: vec![function],
        ..Program::default()
    }
    .into_checked()
    .expect("checked local-flow MIR");

    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("lower local-flow MIR");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("function-scoped scalar locals should be supported")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.contains("const int 7"), "{dump}");
    assert!(dump.contains("const int 11"), "{dump}");
    assert!(dump.contains("const int 13"), "{dump}");
    assert!(dump.contains("branch"), "{dump}");
}

#[test]
fn structurally_recursive_fibonacci_uses_checked_edges_and_recursive_invokes() {
    let dump = complete_dump(
        r"module recursive_fib

fn fibonacci(value Int) Int {
    if value < 2 {
        value
    } else {
        fibonacci(value - 1) + fibonacci(value - 2)
    }
}

pub fn main() Unit {
    let output = fibonacci(8)
    Unit
}
",
    );
    let fibonacci = dump.split("fn i1").next().expect("first function dump");
    assert!(fibonacci.contains("int.compare.less"), "{dump}");
    assert!(
        fibonacci.matches("checked_int.subtract").count() >= 2,
        "{dump}"
    );
    assert!(fibonacci.matches("invoke i0").count() >= 2, "{dump}");
    assert!(fibonacci.contains("checked_int.add"), "{dump}");
    assert!(dump.contains("resume_fault"), "{dump}");
}

#[test]
fn structurally_iterative_fibonacci_lowers_for_range_and_loop_carried_assignments() {
    let dump = complete_dump(
        r"module iterative_fib

fn fibonacci(limit Int) Int {
    var previous = 0
    var current = 1
    for index in 0..limit {
        let next = previous + current
        previous = current
        current = next
        Unit
    }
    previous
}

pub fn main() Unit {
    let output = fibonacci(8)
    Unit
}
",
    );
    assert!(dump.contains("int.compare.less"), "{dump}");
    assert!(dump.contains("checked_int.add"), "{dump}");
    assert!(dump.contains("int.successor_below"), "{dump}");
    assert!(dump.contains("jump b"), "{dump}");
    assert!(dump.contains("invoke i0"), "{dump}");
    assert!(dump.contains("resume_fault"), "{dump}");
}

#[test]
fn assert_and_defer_are_stably_unsupported_until_cleanup_ladders_exist() {
    let mir = compile(
        r"module cleanup

fn check(condition Bool) Unit {
    defer { Unit }
    assert condition
    Unit
}

pub fn main() Unit {
    check(true)
    Unit
}
",
    );
    let outcome = lower_typed_artifact(
        &mir,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("test target"),
    )
    .expect("classify cleanup");
    let LoweringOutcome::Unsupported(report) = outcome else {
        panic!("cleanup forms must not be partially lowered")
    };
    let features = report
        .items()
        .iter()
        .map(loom_codegen_ir::UnsupportedItem::feature)
        .collect::<Vec<_>>();
    assert!(
        features.contains(&UnsupportedFeature::DeferredCleanup),
        "{features:?}"
    );
    assert!(
        features.contains(&UnsupportedFeature::AssertionCleanup),
        "{features:?}"
    );
}

#[test]
fn closed_pod_records_lower_to_products_with_direct_and_fault_writebacks() {
    let dump = complete_dump(
        r"module product_records

record Counter { total Int, calls Int }
record Holder { counter Counter, enabled Bool }

impl Counter {
    method reset(mut self) Unit {
        self.total = 0
        Unit
    }

    method add(mut self, value Int) Unit {
        self.total = self.total + value
        self.calls = self.calls + 1
        Unit
    }
}

impl Holder {
    method setTotal(mut self, value Int) Unit {
        self.counter.total = value
        Unit
    }
}

fn make() Holder {
    var holder = Holder {
        counter = Counter { total = 1, calls = 2 },
        enabled = true,
    }
    holder.setTotal(3)
    holder
}

pub fn main() Unit {
    var counter = Counter { total = 0, calls = 0 }
    counter.reset()
    counter.add(4)
    discard counter.total
    let holder = make()
    discard holder.counter.total
    Unit
}
",
    );

    assert!(dump.contains("product p0(t3, t3)"), "{dump}");
    assert!(dump.contains("product p1(t5, t2)"), "{dump}");
    assert!(dump.contains("registration k5 = Nominal#"), "{dump}");
    assert!(dump.contains("=> t5"), "{dump}");
    assert!(dump.contains("registration k6 = Nominal#"), "{dump}");
    assert!(dump.contains("=> t6"), "{dump}");
    assert!(dump.contains("product.construct"), "{dump}");
    assert!(dump.contains("product.extract"), "{dump}");
    assert!(dump.contains("product.insert"), "{dump}");
    assert!(dump.contains("inout=[0]"), "{dump}");
    assert!(dump.contains("writebacks("), "{dump}");
    assert!(dump.contains(" = call "), "{dump}");
    assert!(dump.contains("invoke "), "{dump}");
}

#[test]
fn structural_tuples_and_records_lower_through_one_direct_aggregate_plan() {
    let dump = complete_dump(
        r"module tuple_products

record Packet { pair (Int, Bool) }

fn rearrange(input (Packet, Float)) (Bool, Packet) {
    let packet, ignored = input
    let number, enabled = packet.pair
    discard ignored
    (enabled, Packet { pair = (number + 1, enabled) })
}

pub fn main() Unit {
    let enabled, packet = rearrange((Packet { pair = (40, true) }, 1.5))
    discard enabled
    let number, copied = packet.pair
    discard number
    discard copied
    Unit
}
",
    );

    assert!(dump.contains("Tuple[Int,Bool]"), "{dump}");
    assert!(dump.contains("Tuple[Nominal#"), "{dump}");
    assert!(dump.contains("Tuple[Bool,Nominal#"), "{dump}");
    assert!(dump.matches("product.construct").count() >= 4, "{dump}");
    assert!(dump.matches("product.extract").count() >= 6, "{dump}");
    assert!(dump.contains("checked_int.add"), "{dump}");
}

#[test]
fn managed_tuple_elements_and_over_budget_tuples_select_atomic_fallback() {
    let managed = lower_run(
        r#"module managed_tuple

fn make() (Int, Text) { (1, "legacy") }

pub fn main() Unit {
    let number, label = make()
    discard number
    discard label
    Unit
}
"#,
    );
    let LoweringOutcome::Unsupported(managed) = managed else {
        panic!("a tuple containing Text must select whole-artifact fallback")
    };
    assert!(managed.items().iter().any(|item| matches!(
        item.feature(),
        UnsupportedFeature::SignatureType
            | UnsupportedFeature::ExpressionType
            | UnsupportedFeature::TextConstant
    )));

    let fields = std::iter::repeat_n("Int", 256)
        .collect::<Vec<_>>()
        .join(", ");
    let values = std::iter::repeat_n("0", 256).collect::<Vec<_>>().join(", ");
    let source = format!(
        "module wide_tuple\n\nfn make() ({fields}) {{ ({values}) }}\n\npub fn main() Unit {{\n    discard make()\n    Unit\n}}\n"
    );
    let wide = lower_run(&source);
    let LoweringOutcome::Unsupported(wide) = wide else {
        panic!("an expanded tuple over the direct-product budget must select fallback")
    };
    assert!(wide.items().iter().any(|item| matches!(
        item.feature(),
        UnsupportedFeature::SignatureType | UnsupportedFeature::ExpressionType
    )));
}

#[test]
fn unsupported_record_boundaries_select_one_atomic_fallback() {
    let managed = lower_run(
        r#"module managed_record

record Message { text Text }

pub fn main() Unit {
    let message = Message { text = "managed" }
    discard message.text
    Unit
}
"#,
    );
    let LoweringOutcome::Unsupported(managed) = managed else {
        panic!("managed record must select fallback")
    };
    assert!(
        managed.items().iter().any(|item| matches!(
            item.feature(),
            UnsupportedFeature::SignatureType
                | UnsupportedFeature::ExpressionType
                | UnsupportedFeature::NominalValue
                | UnsupportedFeature::TextConstant
        )),
        "{managed:?}"
    );

    let invariant = lower_run(
        r"module invariant_record

record Positive {
    value Int
    invariant self.value >= 0
}

fn checked(value Int) Result[Positive, ConstraintError] {
    Positive { value = value }
}

pub fn main() Unit {
    discard checked(1)
    Unit
}
",
    );
    let LoweringOutcome::Unsupported(invariant) = invariant else {
        panic!("invariant/runtime construction must select fallback")
    };
    assert!(
        invariant.items().iter().any(|item| matches!(
            item.feature(),
            UnsupportedFeature::NominalValue
                | UnsupportedFeature::ExpressionType
                | UnsupportedFeature::PatternMatch
        )),
        "{invariant:?}"
    );

    let projected_inout = lower_run(
        r"module projected_inout

record Counter { value Int }
record Holder { counter Counter }

impl Counter {
    method add(mut self, value Int) Unit {
        self.value = self.value + value
        Unit
    }
}

pub fn main() Unit {
    var holder = Holder { counter = Counter { value = 0 } }
    holder.counter.add(1)
    Unit
}
",
    );
    let LoweringOutcome::Unsupported(projected_inout) = projected_inout else {
        panic!("projected inout must select fallback atomically")
    };
    assert!(projected_inout.items().iter().any(|item| {
        item.feature() == UnsupportedFeature::InOutArgument && item.path().contains("arguments[0]")
    }));
    assert!(projected_inout.items().iter().any(|item| {
        item.feature() == UnsupportedFeature::ProjectedPlace
            && item.path().contains("arguments[0].place")
    }));
}

#[test]
#[allow(clippy::too_many_lines)]
fn over_budget_product_depth_and_structure_select_atomic_fallback() {
    use loom_mir::{
        Block, CallArgument, CallPlan, CallTarget, Constant, ConstructionMode, Expr, ExprKind,
        FieldDef, Function, FunctionId, LocalDecl, LocalId, Program, Statement, StatementKind,
        Type, TypeDef, TypeDefKind, TypeId,
    };

    const OVER_BUDGET_RECORDS: usize = 257;
    let span = loom_core::Span::default();
    let nominal = |index: usize| {
        Type::Nominal(
            TypeId(u32::try_from(index).expect("test type identity")),
            Vec::new(),
        )
    };
    let record_type = |index: usize, fields: Vec<FieldDef>| TypeDef {
        id: TypeId(u32::try_from(index).expect("test type identity")),
        name: format!("R{index}"),
        span,
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields,
            invariant: None,
        },
    };
    let function = |id: usize, name: String, result: Type, tail: Expr| {
        let mut function = Function {
            id: FunctionId(u32::try_from(id).expect("test function identity")),
            name,
            span,
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: Vec::new(),
            return_ty: result,
            receiver: None,
            body: Block {
                statements: Vec::new(),
                tail: Some(Box::new(tail)),
                span,
            },
            call_plan: CallPlan::default(),
        };
        function.renumber_expr_ids().expect("number test function");
        function
    };
    let root = |id: usize, record: Type, callee: FunctionId| {
        let call = Expr::new(
            ExprKind::Call {
                target: CallTarget::Direct(callee),
                type_arguments: Vec::new(),
                arguments: Vec::new(),
                witnesses: Vec::new(),
            },
            record.clone(),
            span,
        );
        let mut root = Function {
            id: FunctionId(u32::try_from(id).expect("root identity")),
            name: "manual.main".into(),
            span,
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: vec![LocalDecl {
                id: LocalId(0),
                name: "value".into(),
                ty: record,
                mutable: false,
                span,
            }],
            return_ty: Type::Unit,
            receiver: None,
            body: Block {
                statements: vec![Statement {
                    kind: StatementKind::Let {
                        local: LocalId(0),
                        value: call,
                    },
                    span,
                }],
                tail: Some(Box::new(Expr::new(
                    ExprKind::Constant(Constant::Unit),
                    Type::Unit,
                    span,
                ))),
                span,
            },
            call_plan: CallPlan::default(),
        };
        root.renumber_expr_ids().expect("number root");
        root
    };

    let deep_types = (0..OVER_BUDGET_RECORDS)
        .map(|index| {
            let field = if index + 1 == OVER_BUDGET_RECORDS {
                FieldDef {
                    name: "value".into(),
                    ty: Type::Int,
                    span,
                }
            } else {
                FieldDef {
                    name: "next".into(),
                    ty: nominal(index + 1),
                    span,
                }
            };
            record_type(index, vec![field])
        })
        .collect::<Vec<_>>();
    let mut deep_functions: Vec<Function> = Vec::with_capacity(OVER_BUDGET_RECORDS + 1);
    for index in (0..OVER_BUDGET_RECORDS).rev() {
        let field = if index + 1 == OVER_BUDGET_RECORDS {
            Expr::new(ExprKind::Constant(Constant::Int(0)), Type::Int, span)
        } else {
            let child = deep_functions.last().expect("child factory").id;
            Expr::new(
                ExprKind::Call {
                    target: CallTarget::Direct(child),
                    type_arguments: Vec::new(),
                    arguments: Vec::<CallArgument>::new(),
                    witnesses: Vec::new(),
                },
                nominal(index + 1),
                span,
            )
        };
        let result = nominal(index);
        deep_functions.push(function(
            deep_functions.len(),
            format!("manual.make_r{index}"),
            result.clone(),
            Expr::new(
                ExprKind::Record {
                    ty: TypeId(u32::try_from(index).expect("record identity")),
                    type_arguments: Vec::new(),
                    fields: vec![field],
                    construction: ConstructionMode::Plain,
                },
                result,
                span,
            ),
        ));
    }
    let deep_factory = deep_functions.last().expect("root factory").id;
    deep_functions.push(root(OVER_BUDGET_RECORDS, nominal(0), deep_factory));
    let deep = Program {
        types: deep_types,
        functions: deep_functions,
        exports: BTreeMap::from([(
            "main".into(),
            FunctionId(u32::try_from(OVER_BUDGET_RECORDS).expect("root identity")),
        )]),
        ..Program::default()
    }
    .into_checked()
    .expect("checked deep product graph");

    let wide_id = TypeId(0);
    let wide_types = vec![record_type(
        0,
        (0..OVER_BUDGET_RECORDS)
            .map(|index| FieldDef {
                name: format!("f{index}"),
                ty: Type::Int,
                span,
            })
            .collect(),
    )];
    let wide_type = Type::Nominal(wide_id, Vec::new());
    let wide_fields = (0..OVER_BUDGET_RECORDS)
        .map(|_| Expr::new(ExprKind::Constant(Constant::Int(0)), Type::Int, span))
        .collect();
    let wide_factory = function(
        0,
        "manual.make_wide".into(),
        wide_type.clone(),
        Expr::new(
            ExprKind::Record {
                ty: wide_id,
                type_arguments: Vec::new(),
                fields: wide_fields,
                construction: ConstructionMode::Plain,
            },
            wide_type.clone(),
            span,
        ),
    );
    let wide = Program {
        types: wide_types,
        functions: vec![wide_factory, root(1, wide_type, FunctionId(0))],
        exports: BTreeMap::from([("main".into(), FunctionId(1))]),
        ..Program::default()
    }
    .into_checked()
    .expect("checked wide product graph");

    for mir in [&deep, &wide] {
        let outcome = lower_typed_artifact(
            mir,
            &SourceArtifactRequest::Run {
                entry: "main".into(),
            },
            TargetLayout::new(64).expect("test target"),
        )
        .expect("over-budget product classification");
        let LoweringOutcome::Unsupported(report) = outcome else {
            panic!("an over-budget direct product graph must select atomic fallback")
        };
        assert!(
            report.items().iter().any(|item| matches!(
                item.feature(),
                UnsupportedFeature::SignatureType
                    | UnsupportedFeature::ExpressionType
                    | UnsupportedFeature::NominalValue
            )),
            "{report:?}"
        );
    }
}

use std::collections::BTreeMap;
