use loom_core::{FileId, LOOM_LANGUAGE_VERSION, Name, PackageId};
use loom_hir::{PackageSourceUnit, SourceUnit, lower_files, lower_package_files};
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

fn compile_and_validate(source: &str) -> loom_mir::CheckedProgram {
    compile(source)
}

fn lower_with_standard_resource(source: &str) -> loom_hir::Program {
    let application = parse_with_file(FileId(0), source);
    let resource = parse_with_file(
        FileId(1),
        include_str!("../../../library/standard/src/resource.loom"),
    );
    assert!(
        application.diagnostics().is_empty() && resource.diagnostics().is_empty(),
        "syntax diagnostics: application={:#?} standard={:#?}",
        application.diagnostics(),
        resource.diagnostics()
    );
    let standard = PackageId::compiler_standard(LOOM_LANGUAGE_VERSION);
    let root = PackageId::legacy();
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: FileId(0),
            package: root.clone(),
            syntax: application.ast(),
        },
        PackageSourceUnit {
            file: FileId(1),
            package: standard.clone(),
            syntax: resource.ast(),
        },
    ]);
    lowered
        .program
        .register_package(standard.clone(), [], false);
    lowered
        .program
        .register_package(root, [(Name::new("standard"), standard)], true);
    assert!(
        lowered.diagnostics.is_empty(),
        "HIR diagnostics: {:#?}",
        lowered.diagnostics
    );
    lowered.program
}

fn compile_with_standard_resource(source: &str) -> loom_mir::CheckedProgram {
    let program = lower_with_standard_resource(source);
    let analysis = analyze(&program);
    assert!(
        analysis.diagnostics.is_empty(),
        "semantic diagnostics: {:#?}",
        analysis.diagnostics
    );
    lower_to_mir(&program, &analysis)
        .unwrap_or_else(|failure| panic!("MIR lowering diagnostics: {:#?}", failure.diagnostics()))
}

fn analyze_with_standard_resource(source: &str) -> Vec<loom_core::Diagnostic> {
    let program = lower_with_standard_resource(source);
    analyze(&program).diagnostics
}

fn function_has_name(function: &loom_mir::Function, expected: &str) -> bool {
    function.name.rsplit('.').next() == Some(expected)
}

fn suspension_local_names(function: &loom_mir::Function, state: u32) -> Vec<String> {
    let point = function
        .suspension_points
        .iter()
        .find(|point| point.state == state)
        .expect("suspension state");
    point
        .live_locals
        .iter()
        .map(|id| {
            function
                .params
                .iter()
                .chain(&function.locals)
                .find(|local| local.id == *id)
                .expect("live local declaration")
                .name
                .clone()
        })
        .collect()
}

fn contract_contains_binding(expression: &loom_mir::ContractExpr, expected: u32) -> bool {
    use loom_mir::ContractExprKind;
    match &expression.kind {
        ContractExprKind::Binding(index) => *index == expected,
        ContractExprKind::Field(value, _)
        | ContractExprKind::Unary(_, value)
        | ContractExprKind::IsFinite(value) => contract_contains_binding(value, expected),
        ContractExprKind::Binary(_, left, right) => {
            contract_contains_binding(left, expected) || contract_contains_binding(right, expected)
        }
        ContractExprKind::Match { scrutinee, arms } => {
            contract_contains_binding(scrutinee, expected)
                || arms
                    .iter()
                    .any(|arm| contract_contains_binding(&arm.value, expected))
        }
        ContractExprKind::Constant(_) | ContractExprKind::Value(_) => false,
    }
}

#[test]
fn core01_source_lowers_and_validates() {
    compile_and_validate(include_str!("../../../examples/core01/shop.loom"));
}

#[test]
fn core02_source_lowers_and_validates() {
    let program = compile_and_validate(include_str!("../../../examples/core02/concepts.loom"));
    assert_eq!(program.concepts.len(), 3);
    assert_eq!(program.witnesses.len(), 3);
    assert!(format!("{program:#?}").contains("MakeView"));
}

#[test]
fn core03_source_lowers_and_validates() {
    let program =
        compile_with_standard_resource(include_str!("../../../examples/core03/tasks.loom"));
    assert!(program.functions.iter().any(|function| function.is_async));
    assert!(format!("{program:#?}").contains("Await"));
    assert!(format!("{program:#?}").contains("Tuple"));
    assert!(format!("{program:#?}").contains("LetTuple"));
    assert!(format!("{program:#?}").contains("Defer"));
}

#[test]
fn logical_chains_lower_on_one_mib_stack_and_remain_balanced() {
    let operand_count = loom_syntax::MAX_SYNTAX_NESTING - 8;
    let and_values = (0..operand_count)
        .map(|index| index % 3 != 1)
        .collect::<Vec<_>>();
    let or_values = (0..operand_count)
        .map(|index| index % 4 == 2)
        .collect::<Vec<_>>();
    let and_source = and_values
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(" && ");
    let or_source = or_values
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(" || ");
    let contract_source = std::iter::repeat_n("flag", operand_count)
        .collect::<Vec<_>>()
        .join(" && ");
    let source = format!(
        "module low_stack\n\nfn all() Bool {{ {and_source} }}\n\nfn any() Bool {{ {or_source} }}\n\nfn guarded(flag Bool) Bool\n    requires {contract_source}\n{{\n    flag\n}}\n"
    );

    let program = std::thread::Builder::new()
        .name("loom-low-stack-logical-lowering".into())
        .stack_size(1024 * 1024)
        .spawn(move || compile(&source))
        .expect("spawn compiler on a Windows-sized stack")
        .join()
        .expect("compile near-limit logical chains on a Windows-sized stack");

    for (name, operator, expected) in [
        ("all", loom_mir::BinaryOp::And, and_values),
        ("any", loom_mir::BinaryOp::Or, or_values),
    ] {
        let function = program
            .functions
            .iter()
            .find(|function| function_has_name(function, name))
            .expect("logical-chain function");
        let root = function.body.tail.as_deref().expect("logical-chain tail");
        let mut pending = vec![(root, 1_usize)];
        let mut actual = Vec::new();
        let mut maximum_depth = 0_usize;
        while let Some((expression, depth)) = pending.pop() {
            maximum_depth = maximum_depth.max(depth);
            match &expression.kind {
                loom_mir::ExprKind::Binary(actual_operator, left, right) => {
                    assert_eq!(*actual_operator, operator);
                    pending.push((right, depth + 1));
                    pending.push((left, depth + 1));
                }
                loom_mir::ExprKind::Constant(loom_mir::Constant::Bool(value)) => {
                    actual.push(*value);
                }
                other => panic!("unexpected logical-chain node: {other:#?}"),
            }
        }
        assert_eq!(actual, expected, "logical operand order changed");
        assert!(
            maximum_depth <= 9,
            "logical MIR remained too deep: {maximum_depth}"
        );
    }

    let guarded = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "guarded"))
        .expect("contract function");
    let contract = &guarded.call_plan.requires[0].expression;
    let mut pending = vec![(contract, 1_usize)];
    let mut leaves = 0_usize;
    let mut maximum_depth = 0_usize;
    while let Some((expression, depth)) = pending.pop() {
        maximum_depth = maximum_depth.max(depth);
        match &expression.kind {
            loom_mir::ContractExprKind::Binary(loom_mir::BinaryOp::And, left, right) => {
                pending.push((right, depth + 1));
                pending.push((left, depth + 1));
            }
            loom_mir::ContractExprKind::Value(loom_mir::ContractValue::Argument(0)) => {
                leaves += 1;
            }
            other => panic!("unexpected logical-contract node: {other:#?}"),
        }
    }
    assert_eq!(leaves, operand_count);
    assert!(
        maximum_depth <= 9,
        "logical contract MIR remained too deep: {maximum_depth}"
    );
}

#[test]
fn scoped_source_lowers_to_first_class_mir_without_a_synthetic_defer() {
    let program = compile_with_standard_resource(
        r"
module custom_resource

import standard.resource.Dispose
import standard.resource.MustScope

record Resource {
    value Int
}

impl Dispose for Resource {
    method dispose(mut self) {
        self.value = 0
    }
}

impl MustScope for Resource {}

fn main() {
    scoped resource = Resource { value = 1 }
}
",
    );
    assert!(program.prelude.dispose_concept.is_some());
    assert!(program.prelude.dispose_requirement.is_some());
    assert!(program.prelude.must_scope_concept.is_some());
    assert!(program.prelude.no_suspend_concept.is_some());
    let dispose = program
        .concept(program.prelude.dispose_concept.expect("Dispose id"))
        .expect("Dispose concept");
    let must_scope = program
        .concept(program.prelude.must_scope_concept.expect("MustScope id"))
        .expect("MustScope concept");
    let no_suspend = program
        .concept(program.prelude.no_suspend_concept.expect("NoSuspend id"))
        .expect("NoSuspend concept");
    assert_eq!(dispose.module, "standard.resource");
    assert_eq!(dispose.identity, Some(loom_mir::ConceptIdentity::Dispose));
    assert_eq!(must_scope.module, "standard.resource");
    assert_eq!(
        must_scope.identity,
        Some(loom_mir::ConceptIdentity::MustScope)
    );
    assert_eq!(no_suspend.module, "standard.resource");
    assert_eq!(
        no_suspend.identity,
        Some(loom_mir::ConceptIdentity::NoSuspend)
    );
    let main = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "main"))
        .expect("main function");
    let debug = format!("{:#?}", main.body);
    assert!(debug.contains("Scoped"), "{debug}");
    assert!(debug.contains("StaticConcept"), "{debug}");
    assert!(!debug.contains("Defer"), "{debug}");
}

#[test]
fn source_and_portable_mir_independently_reject_unscoped_must_scope_state() {
    let diagnostics = analyze_with_standard_resource(
        r"
module unscoped_resource

import standard.resource.Dispose
import standard.resource.MustScope

record Resource {
    value Int
}

impl Dispose for Resource {
    method dispose(mut self) {
    }
}

impl MustScope for Resource {}

fn invalid() {
    let resource = Resource { value = 1 }
}
",
    );
    assert_eq!(
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>(),
        ["MustScopeRequiresScoped"]
    );
}

#[test]
fn proof_dispositions_survive_lowering_as_checked_mir_modes() {
    let program = compile_and_validate(
        "module sample\n\ntype Money = Float where self >= 0.0\n\nrecord Range {\n    low Money\n    high Money\n    invariant self.low <= self.high\n}\n\nfn direct_money() Money { Money(10.0) }\n\nfn checked_money(raw Float) Result[Money, ConstraintError] { Money(raw) }\n\nfn direct_range() Range {\n    Range { low = Money(1.0), high = Money(2.0) }\n}\n\nfn checked_range(low Money, high Money) Result[Range, ConstraintError] {\n    Range { low = low, high = high }\n}\n",
    );
    let debug = format!("{program:#?}");
    assert_eq!(debug.matches("construction: Proven").count(), 4, "{debug}");
    assert_eq!(debug.matches("construction: Runtime").count(), 2, "{debug}");
}

#[test]
fn contract_and_assert_proofs_remove_only_established_runtime_checks() {
    let program = compile_and_validate(
        "module sample\n\ntype Money = Float where self >= 0.0\n\nfn established(value Money) Money\n    requires value >= 0.0\n    ensures result >= 0.0\n{\n    assert value >= 0.0\n    Money(value)\n}\n\nfn dynamic(raw Float) Float\n    requires raw >= 0.0\n    ensures result >= 0.0\n{\n    assert raw >= 0.0\n    raw\n}\n\nfn unchecked_assert(raw Float) {\n    assert raw >= 0.0\n}\n",
    );
    let established = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "established"))
        .expect("established function");
    assert!(established.call_plan.requires.is_empty());
    assert!(established.call_plan.ensures.is_empty());
    assert!(
        established
            .body
            .statements
            .iter()
            .all(|statement| !matches!(statement.kind, loom_mir::StatementKind::Assert { .. }))
    );

    let dynamic = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "dynamic"))
        .expect("dynamic function");
    assert_eq!(dynamic.call_plan.requires.len(), 1);
    assert_eq!(dynamic.call_plan.ensures.len(), 1);
    assert!(
        dynamic
            .body
            .statements
            .iter()
            .all(|statement| !matches!(statement.kind, loom_mir::StatementKind::Assert { .. }))
    );

    let unchecked = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "unchecked_assert"))
        .expect("unchecked assertion function");
    assert!(
        unchecked
            .body
            .statements
            .iter()
            .any(|statement| matches!(statement.kind, loom_mir::StatementKind::Assert { .. }))
    );
}

#[test]
fn checked_mir_accepts_the_total_unary_contract_matrix() {
    let program = compile_and_validate(
        "module sample\n\nfn unaryContracts(required Int, returned Int, asserted Int, floating Float, flag Bool) Int\n    requires -required <= 0\n    requires -floating <= 0.0\n    requires !flag\n    ensures -result <= 0\n{\n    assert -asserted <= 0\n    returned\n}\n",
    );
    let function = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "unaryContracts"))
        .expect("unary contract function");

    assert_eq!(function.call_plan.requires.len(), 3);
    assert_eq!(function.call_plan.ensures.len(), 1);
    assert!(matches!(
        function.call_plan.requires[0].expression.kind,
        loom_mir::ContractExprKind::Binary(
            _,
            ref left,
            _
        ) if matches!(left.kind, loom_mir::ContractExprKind::Unary(loom_mir::UnaryOp::Negate, _))
    ));
    assert!(matches!(
        function.call_plan.requires[1].expression.kind,
        loom_mir::ContractExprKind::Binary(
            _,
            ref left,
            _
        ) if matches!(left.kind, loom_mir::ContractExprKind::Unary(loom_mir::UnaryOp::Negate, _))
    ));
    assert!(matches!(
        function.call_plan.requires[2].expression.kind,
        loom_mir::ContractExprKind::Unary(loom_mir::UnaryOp::Not, _)
    ));
    assert!(matches!(
        function.call_plan.ensures[0].expression.kind,
        loom_mir::ContractExprKind::Binary(
            _,
            ref left,
            _
        ) if matches!(left.kind, loom_mir::ContractExprKind::Unary(loom_mir::UnaryOp::Negate, _))
    ));
    assert!(function.body.statements.iter().any(|statement| {
        matches!(
            statement.kind,
            loom_mir::StatementKind::Assert {
                condition: loom_mir::Expr {
                    kind: loom_mir::ExprKind::Binary(
                        _,
                        ref left,
                        _
                    ),
                    ..
                }
            } if matches!(left.kind, loom_mir::ExprKind::Unary(loom_mir::UnaryOp::Negate, _))
        )
    }));
}

#[test]
fn mutable_receiver_requires_only_proves_the_entry_snapshot() {
    let program = compile_and_validate(
        "module sample\n\nrecord Boxed { value Float }\n\nimpl Boxed {\n    method change(mut self)\n        requires self.value >= 0.0\n        ensures old(self.value) >= 0.0\n        ensures self.value >= 0.0\n    {\n        self.value = -1.0\n    }\n}\n",
    );
    let change = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "change"))
        .expect("change method");
    assert_eq!(change.call_plan.requires.len(), 1);
    assert_eq!(
        change.call_plan.ensures.len(),
        1,
        "the entry requires may eliminate old(self), but not mutated current self"
    );
}

#[test]
fn earlier_contract_clauses_eliminate_weaker_later_clauses() {
    let program = compile_and_validate(
        "module sample\n\nfn ordered(value Float) Float\n    requires value >= 0.0\n    requires value >= -1.0\n    ensures result >= 0.0\n    ensures result >= -1.0\n{\n    value\n}\n",
    );
    let ordered = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "ordered"))
        .expect("ordered function");
    assert_eq!(ordered.call_plan.requires.len(), 1);
    assert_eq!(ordered.call_plan.ensures.len(), 1);
}

#[test]
fn lowering_and_artifact_are_deterministic() {
    let source = include_str!("../../../examples/core02/concepts.loom");
    let first = compile_with_standard_resource(source);
    let second = compile_with_standard_resource(source);
    for function in &first.functions {
        assert!(
            function
                .exprs_preorder()
                .enumerate()
                .all(|(expected, expression)| u32::try_from(expected) == Ok(expression.id.0)),
            "lowering must assign canonical dense expression ids in {}",
            function.name
        );
    }
    assert_eq!(
        loom_mir::encode_interpreted_artifact(&first).expect("first artifact"),
        loom_mir::encode_interpreted_artifact(&second).expect("second artifact")
    );
}

#[test]
fn generic_data_and_function_lower_without_monomorphizing() {
    let program = compile_and_validate(
        "module sample\n\nrecord Boxed[T] { value T }\n\nfn wrap[T](value T) Boxed[T] { Boxed { value = value } }\n\ntest fn wraps_text() {\n    let boxed = wrap(\"loom\")\n    assert boxed.value == \"loom\"\n}\n",
    );
    assert!(
        program
            .functions
            .iter()
            .any(|function| function_has_name(function, "wrap") && function.type_parameters == 1)
    );
}

#[test]
fn conditional_conformance_builds_recursive_proof_application() {
    let program = compile_and_validate(
        "module sample\n\nconcept Equivalent {\n    method equivalent(self, other Self) Bool\n}\n\nrecord Atom { value Int }\n\nimpl Equivalent for Atom {\n    method equivalent(self, other Atom) Bool { self.value == other.value }\n}\n\nrecord Boxed[T] { value T }\n\nimpl[T: Equivalent] Equivalent for Boxed[T] {\n    method equivalent(self, other Boxed[T]) Bool {\n        self.value.equivalent(other.value)\n    }\n}\n\nfn same[T: Equivalent](left T, right T) Bool {\n    left.equivalent(right)\n}\n\ntest fn conditional_witness() {\n    let left = Boxed { value = Atom { value = 7 } }\n    let right = Boxed { value = Atom { value = 7 } }\n    let equal = same(left, right)\n    assert equal\n}\n",
    );
    assert!(
        program
            .witnesses
            .iter()
            .any(|witness| !witness.prerequisites.is_empty())
    );
    for witness in &program.witnesses {
        for method in witness.methods.values() {
            assert_eq!(
                program.functions[method.0 as usize].witness_prefix_count,
                u32::try_from(witness.prerequisites.len()).expect("test witness arity")
            );
        }
    }
    assert!(program.functions.iter().all(|function| {
        let is_witness_method = program.witnesses.iter().any(|witness| {
            witness
                .methods
                .values()
                .any(|method| *method == function.id)
        });
        is_witness_method || function.witness_prefix_count == 0
    }));
    assert!(format!("{program:#?}").contains("Apply"));
}

#[test]
fn generic_associated_projection_is_preserved_in_function_mir() {
    let program = compile_and_validate(
        "module sample\n\nconcept Source {\n    associated type Item\n    method first(self) Self.Item\n}\n\nrecord Number { value Int }\n\nimpl Source for Number {\n    associated type Item = Int\n    method first(self) Int { self.value }\n}\n\nfn read[T: Source](source T) T.Item { source.first() }\n\ntest fn reads_associated() {\n    let value = read(Number { value = 3 })\n    assert value == 3\n}\n",
    );
    assert!(program.functions.iter().any(|function| matches!(
        function.return_ty,
        loom_mir::Type::AssociatedProjection { .. }
    )));
}

#[test]
fn nested_contract_match_bindings_use_lexical_slots() {
    let program = compile_and_validate(
        "module sample\n\nenum Problem { Failed }\n\nfn keep(value Option[Int]) Result[Option[Int], Problem]\n    ensures match result {\n        Ok(option) => match option {\n            Some(number) => number >= 0\n            None => true\n        }\n        Err(_) => true\n    }\n{\n    Ok(value)\n}\n",
    );
    let function = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "keep"))
        .expect("keep function");
    let contract = &function.call_plan.ensures[0];
    assert!(contract_contains_binding(&contract.expression, 1));
}

#[test]
fn returning_branch_preserves_never_flow() {
    compile_and_validate(
        "module sample\n\nfn choose(flag Bool) Int {\n    if flag {\n        return 0\n    } else {\n        1\n    }\n}\n",
    );
}

#[test]
fn generic_witness_and_method_generics_keep_separate_alpha_spaces() {
    let program = compile_and_validate(
        "module sample\n\nrecord Boxed[T] { value T }\nrecord Holder[T] { value T }\n\nconcept Wrapper {\n    method wrap[U](self, value U) Boxed[U]\n}\n\nimpl[T] Wrapper for Holder[T] {\n    method wrap[U](self, value U) Boxed[U] { Boxed { value = value } }\n}\n\ntest fn generic_method() {\n    let holder = Holder { value = 1 }\n    let boxed = holder.wrap(\"loom\")\n    assert boxed.value == \"loom\"\n}\n",
    );
    let witness = program.witnesses.first().expect("generic witness");
    assert_eq!(witness.type_parameters, 1);
    let requirement = program.requirements.first().expect("generic requirement");
    assert_eq!(requirement.method_type_parameters, 1);
    let method = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "wrap"))
        .expect("conformance method");
    assert_eq!(method.type_parameters, 2);
    assert!(format!("{program:#?}").contains("type_arguments"));
}

#[test]
fn phantom_generic_type_keeps_declared_arity() {
    let program = compile_and_validate(
        "module sample\n\nrecord Marker[T] {}\n\nfn marker[T]() Marker[T] { Marker {} }\n",
    );
    let marker = program
        .types
        .iter()
        .find(|definition| definition.name == "Marker")
        .expect("Marker type");
    assert_eq!(marker.type_parameters, 1);
}

#[test]
fn generic_enum_construction_carries_its_instantiation() {
    let program = compile_and_validate(
        "module sample\n\nenum Choice[T] {\n    Empty\n    Value(T)\n}\n\nfn choose[T](value T) Choice[T] { Choice.Value(value) }\n\ntest fn chooses_text() {\n    match choose(\"loom\") {\n        Value(text) => {\n            assert text == \"loom\"\n            Unit\n        }\n        Empty => {\n            assert false\n            Unit\n        }\n    }\n}\n",
    );
    let choose = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "choose"))
        .expect("choose function");
    assert_eq!(choose.type_parameters, 1);
}

#[test]
fn compiler_builtins_lower_to_validated_calls() {
    compile_and_validate(
        "module sample\n\nimport standard.float.parse_float\nimport standard.float.format_float\n\ntest fn float_text_boundary() {\n    let parsed = parse_float(\"1.25\")\n    let rendered = format_float(1.25)\n    assert rendered == \"1.25\"\n    match parsed {\n        Ok(value) => {\n            assert value == 1.25\n            Unit\n        }\n        Err(standard.float.ParseFloatError.InvalidSyntax) => {\n            assert false\n            Unit\n        }\n        Err(standard.float.ParseFloatError.OutOfRange) => {\n            assert false\n            Unit\n        }\n    }\n}\n",
    );
}

#[test]
fn errored_analysis_returns_only_structured_compiler_defects() {
    let parsed = parse_with_file(
        FileId(0),
        "module sample\n\nfn invalid() Int { missing_name }\n",
    );
    assert!(parsed.diagnostics().is_empty());
    let lowered = lower_files([SourceUnit {
        file: FileId(0),
        syntax: parsed.ast(),
    }]);
    assert!(lowered.diagnostics.is_empty());
    let analysis = analyze(&lowered.program);
    assert!(analysis.has_errors());
    let failure = lower_to_mir(&lowered.program, &analysis).expect_err("must not emit partial MIR");
    assert_eq!(failure.diagnostics().len(), 1);
    assert_eq!(failure.diagnostics()[0].code, "CompilerDefect");
}

#[test]
fn concept_contracts_are_instantiated_for_conformance_methods() {
    compile_and_validate(
        "module sample\n\nconcept Source {\n    associated type Item\n    method first(self) Option[Self.Item]\n        ensures match result {\n            Some(value) => value == value\n            None => true\n        }\n}\n\nrecord Number { value Int }\n\nimpl Source for Number {\n    associated type Item = Int\n    method first(self) Option[Int] { Some(self.value) }\n}\n",
    );
}

#[test]
fn generic_concept_contracts_use_the_implementation_alpha_space() {
    let program = compile_and_validate(
        "module sample\n\nconcept Echo {\n    method echo[U](self, value U) Option[U]\n        ensures match result {\n            Some(output) => true\n            None => true\n        }\n}\n\nrecord Holder[T] { value T }\n\nimpl[T] Echo for Holder[T] {\n    method echo[U](self, value U) Option[U] { Some(value) }\n}\n",
    );
    let method = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "echo"))
        .expect("echo conformance method");
    assert_eq!(method.type_parameters, 2);
    let loom_mir::ContractExprKind::Match { arms, .. } =
        &method.call_plan.ensures[0].expression.kind
    else {
        panic!("expected concept postcondition match");
    };
    assert_eq!(arms[0].bindings, vec![loom_mir::Type::Parameter(1)]);
}

#[test]
fn generic_receiver_invariant_is_instantiated_from_the_impl_target() {
    let program = compile_and_validate(
        "module sample\n\nrecord Pair[A, B] {\n    left Option[A]\n    right Option[B]\n\n    invariant match self.left {\n        Some(value) => true\n        None => true\n    }\n}\n\nimpl[X, Y] Pair[Y, X] {\n    method touch(self) {}\n}\n",
    );
    let method = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "touch"))
        .expect("touch inherent method");
    let invariant = method
        .call_plan
        .receiver_invariant
        .as_ref()
        .expect("receiver invariant");
    let loom_mir::ContractExprKind::Match { arms, .. } = &invariant.expression.kind else {
        panic!("expected invariant match");
    };
    assert_eq!(arms[0].bindings, vec![loom_mir::Type::Parameter(1)]);
}

#[test]
fn old_match_bindings_remain_lexical_snapshot_values() {
    compile_and_validate(
        "module sample\n\nfn keep(value Option[Int]) Option[Int]\n    ensures old(match value {\n        Some(number) => number >= 0,\n        None => true,\n    })\n{\n    value\n}\n",
    );
}

#[test]
fn receiverless_static_requirement_carries_an_explicit_dispatch_type() {
    compile_and_validate(
        "module sample\n\nconcept Zero {\n    static method zero() Self\n}\n\nimpl Zero for Int {\n    static method zero() Int { 0 }\n}\n\nfn make_zero[T: Zero]() T { <T as Zero>.zero() }\n\ntest fn makes_zero() {\n    let zero = make_zero[Int]()\n    assert zero == 0\n}\n",
    );
}

#[test]
fn requirement_method_bound_associated_projection_is_preserved() {
    let program = compile_and_validate(
        "module sample\n\nconcept Source {\n    associated type Item\n    method first(self) Self.Item\n}\n\nrecord Number { value Int }\n\nimpl Source for Number {\n    associated type Item = Int\n    method first(self) Int { self.value }\n}\n\nconcept Mapper {\n    method map[U: Source](self, source U) U.Item\n}\n\nrecord Identity {}\n\nimpl Mapper for Identity {\n    method map[U: Source](self, source U) U.Item { source.first() }\n}\n",
    );
    let mapper = program
        .concepts
        .iter()
        .find(|concept| concept.name == "Mapper")
        .expect("Mapper concept");
    let witness = program
        .witnesses
        .iter()
        .find(|witness| witness.concept == mapper.id)
        .expect("Mapper witness");
    let method = program
        .functions
        .iter()
        .find(|function| {
            witness
                .methods
                .values()
                .any(|candidate| *candidate == function.id)
        })
        .expect("Mapper method");
    assert_eq!(method.witness_prefix_count, 0);
    assert_eq!(method.witness_params.len(), 1);
}

#[test]
fn generic_async_functions_with_witnesses_and_contracts_lower_to_checked_mir() {
    let program = compile_and_validate(
        r"
module sample

concept Measure {
    method measure(self) Int
}

record Number {
    value Int
}

impl Measure for Number {
    method measure(self) Int { self.value }
}

async fn measured[T: Measure](value T, minimum Int) Int
    requires minimum >= 0
    ensures result >= minimum
{
    Task.sleep(0).await
    value.measure() + minimum
}

test async fn generic_async_contracts() {
    let observed = measured(Number { value = 2 }, 3).await
    assert observed == 5
}
",
    );
    let measured = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "measured"))
        .expect("generic async function");
    assert!(measured.is_async);
    assert_eq!(measured.type_parameters, 1);
    assert_eq!(measured.witness_params.len(), 1);
    assert_eq!(measured.call_plan.requires.len(), 1);
    assert_eq!(measured.call_plan.ensures.len(), 1);
    assert_eq!(measured.suspension_points.len(), 1);
}

#[test]
fn only_resolved_task_standard_items_specialize_to_task_mir() {
    let canonical = compile_and_validate(
        r"
module canonical_task_items

async fn child() Int { 1 }

pub async fn main() {
    discard Task.sleep(0).await
    discard Task.all(child()).await
    discard Task.settled(child()).await
    discard Task.any(child()).await
    discard Task.race(child()).await
}
",
    );
    let canonical_dump = format!("{canonical:#?}");
    assert!(canonical_dump.contains("Sleep"));
    assert!(canonical_dump.contains("TaskJoin"));
    assert!(canonical_dump.contains("Settled"));
    assert!(canonical_dump.contains("Any"));
    assert!(canonical_dump.contains("Race"));

    let shadowed = compile_and_validate(
        r"
module shadowed_task_items

record Scheduler {}

impl Scheduler {
    method all(self, value Int) Int { value }
    method settled(self, value Int) Int { value }
    method any(self, value Int) Int { value }
    method race(self, value Int) Int { value }
    method sleep(self, value Int) Int { value }
}

pub fn main() {
    let Task = Scheduler {}
    discard Task.all(1)
    discard Task.settled(2)
    discard Task.any(3)
    discard Task.race(4)
    discard Task.sleep(5)
}
",
    );
    let shadowed_dump = format!("{shadowed:#?}");
    assert!(!shadowed_dump.contains("TaskJoin"));
    assert!(!shadowed_dump.contains("Sleep {"));
    assert!(shadowed_dump.contains("Inherent"));
}

#[test]
fn async_exit_contracts_add_only_referenced_parameters_to_suspension_metadata() {
    let program = compile_and_validate(
        r"
module exit_contract_liveness

async fn constrained(ignored Int, required Int, oldRequired Int) Int
    ensures result >= required && old(oldRequired) == oldRequired
{
    Task.sleep(0).await
    7
}

test async fn callConstrained() {
    discard constrained(99, 3, 4).await
}
",
    );
    let constrained = program
        .functions
        .iter()
        .find(|function| function_has_name(function, "constrained"))
        .expect("contracted async function");

    assert_eq!(
        suspension_local_names(constrained, 1),
        ["required", "oldRequired"],
        "an unreferenced parameter must not enter the coroutine frame"
    );
}

#[test]
fn async_lowering_computes_path_and_cleanup_sensitive_suspension_liveness() {
    let program = compile_and_validate(
        r"
module liveness

enum Input {
    First(Int)
    Second(Int)
}

async fn valueTask(value Int) Int { value }

async fn branches(flag Bool, left Int, right Int) Int {
    let selected = if flag {
        Task.sleep(0).await
        left
    } else {
        Task.sleep(0).await
        right
    }
    Task.sleep(0).await
    selected
}

async fn matches(input Input, extra Int) Int {
    match input {
        First(value) => {
            Task.sleep(0).await
            value
        }
        Second(value) => {
            Task.sleep(0).await
            value + extra
        }
    }
}

async fn loops(limit Int, seed Int, after Int) Int {
    var sum = seed
    for index in 0..limit {
        Task.sleep(0).await
        sum = sum + index
        Unit
    }
    sum + after
}

async fn cleaned(value Int, replacement Int) Int {
    var held = value
    defer {
        held = held + 1
    }
    Task.sleep(0).await
    held = replacement
    held
}

async fn returns(flag Bool, first Int, second Int) Int {
    if flag {
        return valueTask(first).await
    } else {
        Unit
    }
    Task.sleep(0).await
    second
}
",
    );
    let function = |name| {
        program
            .functions
            .iter()
            .find(|function| function_has_name(function, name))
            .expect("async test function")
    };

    let branches = function("branches");
    assert_eq!(suspension_local_names(branches, 1), ["left"]);
    assert_eq!(suspension_local_names(branches, 2), ["right"]);
    assert_eq!(suspension_local_names(branches, 3), ["selected"]);

    let matches = function("matches");
    assert_eq!(suspension_local_names(matches, 1), ["value"]);
    assert_eq!(suspension_local_names(matches, 2), ["extra", "value"]);

    let loops = function("loops");
    assert_eq!(suspension_local_names(loops, 1), ["after", "sum", "index"]);

    let cleaned = function("cleaned");
    assert_eq!(suspension_local_names(cleaned, 1), ["replacement", "held"]);

    let returns = function("returns");
    assert!(suspension_local_names(returns, 1).is_empty());
    assert_eq!(suspension_local_names(returns, 2), ["second"]);

    for function in [branches, matches, loops, cleaned, returns] {
        for point in &function.suspension_points {
            assert!(
                point.live_locals.windows(2).all(|pair| pair[0] < pair[1]),
                "{} state #{} is not stable: {:?}",
                function.name,
                point.state,
                point.live_locals
            );
        }
    }
}

#[test]
fn text_bytes_path_and_path_file_calls_lower_to_checked_mir() {
    let program = compile_and_validate(
        r#"
module standard_value_lowering

import standard.file.open_read_path
import standard.file.create_path

fn values(text Text, bytes Bytes, base Path, child Path, index Int) {
    let scalar_count = text.length()
    let scalar = text.get(index)
    let concatenated = text.concat("!")
    let contained = text.contains("loom")
    let encoded = text.encode_utf8()
    let rebuilt = Text.from_utf8_units([65, 231, 149, 140])
    let byte_count = bytes.length()
    let byte = bytes.get(index)
    let appended = bytes.append(encoded)
    let decoded = appended.decode_utf8()
    let rendered = base.as_text()
    let joined = base.join(child)
    let parsed = Path.from_text(text)
    assert bytes == bytes
    assert base == base
}

fn decodeOutcome(value Result[Text, DecodeTextError]) {
    match value {
        Ok(_) => Unit
        Err(error) => match error {
            InvalidUtf8 => Unit
        }
    }
}

fn pathOutcome(value Result[Path, PathError]) {
    match value {
        Ok(_) => Unit
        Err(error) => match error {
            ContainsNul => Unit
            AbsoluteJoin => Unit
        }
    }
}

async fn pathFiles(path Path) {
    scoped input = open_read_path(path).await
    scoped output = create_path(path).await
}
"#,
    );
    let debug = format!("{program:#?}");
    for builtin in [
        "TextLength",
        "TextGet",
        "TextConcat",
        "TextContains",
        "TextEncodeUtf8",
        "TextFromUtf8Units",
        "BytesLength",
        "BytesGet",
        "BytesAppend",
        "BytesDecodeUtf8",
        "PathFromText",
        "PathAsText",
        "PathJoin",
        "FileOpenReadPath",
        "FileCreatePath",
    ] {
        assert!(debug.contains(builtin), "missing {builtin} in {debug}");
    }
    assert_eq!(debug.matches("Scoped").count(), 2, "{debug}");
    assert!(!debug.contains("Defer"), "{debug}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn structured_standard_values_lower_to_checked_mir() {
    let program = compile_and_validate(
        r#"
module structured_standard_lowering

import standard.file.try_open_read_path
import standard.file.try_create_path
import standard.net.try_connect
import standard.json.parse_json
import standard.json.format_json
import standard.log.debug
import standard.log.info
import standard.log.warn
import standard.log.error
import standard.log.write

fn values(text Text) {
    let fields = TextMap[Text]().insert("name", text).remove("absent")
    let count = fields.length()
    let present = fields.contains("name")
    let value = fields.get("name")
    let first = fields.entry_at(0)
    let null = Json.Null
    let boolean = Json.Bool(true)
    let number = Json.Number(1.5)
    let string = Json.Text(text)
    let array = Json.Array([null, boolean])
    let object = Json.Object(TextMap[Json]().insert("answer", number))
    let parsed = parse_json("null")
    let formatted = format_json(object)
    let syntax = JsonError.InvalidSyntax(2)
    let depth = JsonError.DepthLimit
    debug("debug")
    info("info")
    warn("warn")
    error("error")
    write(LogLevel.Info, "event", fields)
}

fn jsonValue(value Json) {
    match value {
        Null => Unit
        Bool(_) => Unit
        Number(_) => Unit
        Text(_) => Unit
        Array(_) => Unit
        Object(_) => Unit
    }
}

fn jsonFailure(value JsonError) {
    match value {
        InvalidSyntax(_) => Unit
        NumberOutOfRange(_) => Unit
        DepthLimit => Unit
        NonFiniteNumber => Unit
    }
}

fn ioFailure(error IoError) {
    let message = error.message()
    match error.kind() {
        NotFound => Unit
        PermissionDenied => Unit
        AlreadyExists => Unit
        InvalidInput => Unit
        ConnectionRefused => Unit
        ConnectionReset => Unit
        TimedOut => Unit
        UnexpectedEof => Unit
        Closed => Unit
        Other => Unit
    }
}

async fn files(path Path) Result[Unit, IoError] {
    scoped input = try_open_read_path(path).await?
    let content = input.try_read_text().await?
    scoped output = try_create_path(path).await?
    output.try_write_text(content).await?
    Ok(Unit)
}

async fn network(host Text, port Int) Result[Unit, IoError] {
    scoped socket = try_connect(host, port).await?
    socket.try_write_text("ping").await?
    let response = socket.try_read_text().await?
    Ok(Unit)
}
"#,
    );
    let debug = format!("{program:#?}");
    for builtin in [
        "TextMapNew",
        "TextMapLength",
        "TextMapContains",
        "TextMapGet",
        "TextMapEntryAt",
        "TextMapInsert",
        "TextMapRemove",
        "JsonParse",
        "JsonFormat",
        "IoErrorKind",
        "IoErrorMessage",
        "FileTryOpenReadPath",
        "FileTryCreatePath",
        "FileTryReadText",
        "FileTryWriteText",
        "SocketTryConnect",
        "SocketTryReadText",
        "SocketTryWriteText",
        "LogDebug",
        "LogInfo",
        "LogWarn",
        "LogError",
        "LogWrite",
    ] {
        assert!(debug.contains(builtin), "missing {builtin} in {debug}");
    }
}
