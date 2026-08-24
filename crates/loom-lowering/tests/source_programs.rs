use loom_core::FileId;
use loom_hir::{SourceUnit, lower_files};
use loom_lowering::lower_to_mir;
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fn compile(source: &str) -> loom_mir::Program {
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

fn compile_and_validate(source: &str) -> loom_mir::Program {
    let program = compile(source);
    program.validate().expect("lowered MIR validates");
    program
}

fn function_has_name(function: &loom_mir::Function, expected: &str) -> bool {
    function.name.rsplit('.').next() == Some(expected)
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
    let program = compile_and_validate(include_str!("../../../examples/core03/tasks.loom"));
    assert!(program.functions.iter().any(|function| function.is_async));
    assert!(format!("{program:#?}").contains("Await"));
    assert!(format!("{program:#?}").contains("Tuple"));
    assert!(format!("{program:#?}").contains("LetTuple"));
    assert!(format!("{program:#?}").contains("Defer"));
}

#[test]
fn lowering_and_artifact_are_deterministic() {
    let source = include_str!("../../../examples/core02/concepts.loom");
    let first = compile(source);
    let second = compile(source);
    assert_eq!(
        loom_mir::encode_interpreted_artifact(&first).expect("first artifact"),
        loom_mir::encode_interpreted_artifact(&second).expect("second artifact")
    );
}

#[test]
fn generic_data_and_function_lower_without_monomorphizing() {
    let program = compile_and_validate(
        "module sample\n\nrecord Boxed[T] { value T }\n\nfn wrap[T](value T) Boxed[T] { Boxed { value = value } }\n\ntest fn wraps_text() {\n    let boxed = wrap(\"loom\")\n    assert boxed.value == \"loom\"\n    Unit\n}\n",
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
        "module sample\n\nconcept Equivalent {\n    method equivalent(self, other Self) Bool\n}\n\nrecord Atom { value Int }\n\nimpl Equivalent for Atom {\n    method equivalent(self, other Atom) Bool { self.value == other.value }\n}\n\nrecord Boxed[T] { value T }\n\nimpl[T: Equivalent] Equivalent for Boxed[T] {\n    method equivalent(self, other Boxed[T]) Bool {\n        self.value.equivalent(other.value)\n    }\n}\n\nfn same[T: Equivalent](left T, right T) Bool {\n    left.equivalent(right)\n}\n\ntest fn conditional_witness() {\n    let left = Boxed { value = Atom { value = 7 } }\n    let right = Boxed { value = Atom { value = 7 } }\n    let equal = same(left, right)\n    assert equal\n    Unit\n}\n",
    );
    assert!(
        program
            .witnesses
            .iter()
            .any(|witness| !witness.prerequisites.is_empty())
    );
    assert!(format!("{program:#?}").contains("Apply"));
}

#[test]
fn generic_associated_projection_is_preserved_in_function_mir() {
    let program = compile_and_validate(
        "module sample\n\nconcept Source {\n    associated type Item\n    method first(self) Self.Item\n}\n\nrecord Number { value Int }\n\nimpl Source for Number {\n    associated type Item = Int\n    method first(self) Int { self.value }\n}\n\nfn read[T: Source](source T) T.Item { source.first() }\n\ntest fn reads_associated() {\n    let value = read(Number { value = 3 })\n    assert value == 3\n    Unit\n}\n",
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
        "module sample\n\nrecord Boxed[T] { value T }\nrecord Holder[T] { value T }\n\nconcept Wrapper {\n    method wrap[U](self, value U) Boxed[U]\n}\n\nimpl[T] Wrapper for Holder[T] {\n    method wrap[U](self, value U) Boxed[U] { Boxed { value = value } }\n}\n\ntest fn generic_method() {\n    let holder = Holder { value = 1 }\n    let boxed = holder.wrap(\"loom\")\n    assert boxed.value == \"loom\"\n    Unit\n}\n",
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
        "module sample\n\nrecord Pair[A, B] {\n    left Option[A]\n    right Option[B]\n\n    invariant match self.left {\n        Some(value) => true\n        None => true\n    }\n}\n\nimpl[X, Y] Pair[Y, X] {\n    method touch(self) Unit { Unit }\n}\n",
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
        "module sample\n\nconcept Zero {\n    static method zero() Self\n}\n\nimpl Zero for Int {\n    static method zero() Int { 0 }\n}\n\nfn make_zero[T: Zero]() T { <T as Zero>.zero() }\n\ntest fn makes_zero() {\n    let zero = make_zero[Int]()\n    assert zero == 0\n    Unit\n}\n",
    );
}

#[test]
fn requirement_method_bound_associated_projection_is_preserved() {
    compile_and_validate(
        "module sample\n\nconcept Source {\n    associated type Item\n    method first(self) Self.Item\n}\n\nrecord Number { value Int }\n\nimpl Source for Number {\n    associated type Item = Int\n    method first(self) Int { self.value }\n}\n\nconcept Mapper {\n    method map[U: Source](self, source U) U.Item\n}\n\nrecord Identity {}\n\nimpl Mapper for Identity {\n    method map[U: Source](self, source U) U.Item { source.first() }\n}\n",
    );
}
