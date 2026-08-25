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
        "module sample\n\ntype Money = Float where self >= 0.0\n\nfn established(value Money) Money\n    requires value >= 0.0\n    ensures result >= 0.0\n{\n    assert value >= 0.0\n    Money(value)\n}\n\nfn dynamic(raw Float) Float\n    requires raw >= 0.0\n    ensures result >= 0.0\n{\n    assert raw >= 0.0\n    raw\n}\n\nfn unchecked_assert(raw Float) Unit {\n    assert raw >= 0.0\n    Unit\n}\n",
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
fn mutable_receiver_requires_only_proves_the_entry_snapshot() {
    let program = compile_and_validate(
        "module sample\n\nrecord Boxed { value Float }\n\nimpl Boxed {\n    method change(mut self) Unit\n        requires self.value >= 0.0\n        ensures old(self.value) >= 0.0\n        ensures self.value >= 0.0\n    {\n        self.value = -1.0\n        Unit\n    }\n}\n",
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
    Unit
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
fn text_bytes_path_and_path_file_calls_lower_to_checked_mir() {
    let program = compile_and_validate(
        r#"
module standard.resource

import standard.file.open_read_path
import standard.file.create_path

concept Dispose {
    method dispose(mut self) Unit
}

concept MustScope {}
concept NoSuspend {}

fn values(text Text, bytes Bytes, base Path, child Path, index Int) Unit {
    let scalar_count = text.length()
    let scalar = text.get(index)
    let concatenated = text.concat("!")
    let contained = text.contains("loom")
    let encoded = text.encode_utf8()
    let byte_count = bytes.length()
    let byte = bytes.get(index)
    let appended = bytes.append(encoded)
    let decoded = appended.decode_utf8()
    let rendered = base.as_text()
    let joined = base.join(child)
    let parsed = Path.from_text(text)
    assert bytes == bytes
    assert base == base
    Unit
}

fn decodeOutcome(value Result[Text, DecodeTextError]) Unit {
    match value {
        Ok(_) => Unit
        Err(error) => match error {
            InvalidUtf8 => Unit
        }
    }
}

fn pathOutcome(value Result[Path, PathError]) Unit {
    match value {
        Ok(_) => Unit
        Err(error) => match error {
            ContainsNul => Unit
            AbsoluteJoin => Unit
        }
    }
}

async fn pathFiles(path Path) Unit {
    scoped input = open_read_path(path).await
    scoped output = create_path(path).await
    Unit
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
}

#[test]
fn structured_standard_values_lower_to_checked_mir() {
    let program = compile_and_validate(
        r#"
module standard.resource

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

concept Dispose {
    method dispose(mut self) Unit
}

concept MustScope {}
concept NoSuspend {}

fn values(text Text) Unit {
    let fields = TextMap[Text]().insert("name", text).remove("absent")
    let count = fields.length()
    let present = fields.contains("name")
    let value = fields.get("name")
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
    Unit
}

fn jsonValue(value Json) Unit {
    match value {
        Null => Unit
        Bool(_) => Unit
        Number(_) => Unit
        Text(_) => Unit
        Array(_) => Unit
        Object(_) => Unit
    }
}

fn jsonFailure(value JsonError) Unit {
    match value {
        InvalidSyntax(_) => Unit
        NumberOutOfRange(_) => Unit
        DepthLimit => Unit
        NonFiniteNumber => Unit
    }
}

fn ioFailure(error IoError) Unit {
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
