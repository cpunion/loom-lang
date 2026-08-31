use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{DefId, DefinitionKind, PackageSourceUnit, Program, lower_package_files};
use loom_sema::{Analysis, BuiltinValue, CallTarget, Resolution, TyData, analyze};
use loom_syntax::parse_with_file;

#[expect(
    clippy::too_many_lines,
    reason = "the test helper assembles the complete canonical std graph used by builtin-value cases"
)]
fn analyze_source_program(source: &str) -> (Program, Analysis) {
    let root_file = FileId(0);
    let std_log_file = FileId(1);
    let std_json_file = FileId(2);
    let std_float_file = FileId(3);
    let std_text_file = FileId(4);
    let std_path_file = FileId(5);
    let std_file_file = FileId(6);
    let std_net_file = FileId(7);
    let std_io_file = FileId(8);
    let std_resource_file = FileId(9);
    let parsed = parse_with_file(root_file, source);
    let std_log = parse_with_file(
        std_log_file,
        include_str!("../../../library/std/log/log.loom"),
    );
    let std_json = parse_with_file(
        std_json_file,
        include_str!("../../../library/std/json/json.loom"),
    );
    let std_float = parse_with_file(
        std_float_file,
        include_str!("../../../library/std/float/float.loom"),
    );
    let std_text = parse_with_file(
        std_text_file,
        include_str!("../../../library/std/text/text.loom"),
    );
    let std_path = parse_with_file(
        std_path_file,
        include_str!("../../../library/std/path/path.loom"),
    );
    let std_file = parse_with_file(
        std_file_file,
        include_str!("../../../library/std/file/file.loom"),
    );
    let std_net = parse_with_file(
        std_net_file,
        include_str!("../../../library/std/net/net.loom"),
    );
    let std_io = parse_with_file(std_io_file, include_str!("../../../library/std/io/io.loom"));
    let std_resource = parse_with_file(
        std_resource_file,
        include_str!("../../../library/std/resource/resource.loom"),
    );
    assert!(
        parsed.diagnostics().is_empty(),
        "syntax diagnostics: {:#?}",
        parsed.diagnostics()
    );
    assert!(
        std_log.diagnostics().is_empty()
            && std_json.diagnostics().is_empty()
            && std_float.diagnostics().is_empty()
            && std_text.diagnostics().is_empty()
            && std_path.diagnostics().is_empty()
            && std_file.diagnostics().is_empty()
            && std_net.diagnostics().is_empty()
            && std_io.diagnostics().is_empty()
            && std_resource.diagnostics().is_empty(),
        "standard syntax diagnostics: log={:#?} json={:#?} float={:#?} text={:#?} path={:#?} file={:#?} net={:#?} io={:#?} resource={:#?}",
        std_log.diagnostics(),
        std_json.diagnostics(),
        std_float.diagnostics(),
        std_text.diagnostics(),
        std_path.diagnostics(),
        std_file.diagnostics(),
        std_net.diagnostics(),
        std_io.diagnostics(),
        std_resource.diagnostics()
    );
    let root_package = PackageId::new("sema-test", "0");
    let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: root_file,
            package: root_package.clone(),
            module: ModuleName::new("sema_test"),
            syntax: parsed.ast(),
        },
        PackageSourceUnit {
            file: std_log_file,
            package: std_package.clone(),
            module: ModuleName::new("std.log"),
            syntax: std_log.ast(),
        },
        PackageSourceUnit {
            file: std_json_file,
            package: std_package.clone(),
            module: ModuleName::new("std.json"),
            syntax: std_json.ast(),
        },
        PackageSourceUnit {
            file: std_float_file,
            package: std_package.clone(),
            module: ModuleName::new("std.float"),
            syntax: std_float.ast(),
        },
        PackageSourceUnit {
            file: std_text_file,
            package: std_package.clone(),
            module: ModuleName::new("std.text"),
            syntax: std_text.ast(),
        },
        PackageSourceUnit {
            file: std_path_file,
            package: std_package.clone(),
            module: ModuleName::new("std.path"),
            syntax: std_path.ast(),
        },
        PackageSourceUnit {
            file: std_file_file,
            package: std_package.clone(),
            module: ModuleName::new("std.file"),
            syntax: std_file.ast(),
        },
        PackageSourceUnit {
            file: std_net_file,
            package: std_package.clone(),
            module: ModuleName::new("std.net"),
            syntax: std_net.ast(),
        },
        PackageSourceUnit {
            file: std_io_file,
            package: std_package.clone(),
            module: ModuleName::new("std.io"),
            syntax: std_io.ast(),
        },
        PackageSourceUnit {
            file: std_resource_file,
            package: std_package.clone(),
            module: ModuleName::new("std.resource"),
            syntax: std_resource.ast(),
        },
    ]);
    assert!(
        lowered.diagnostics.is_empty(),
        "HIR diagnostics: {:#?}",
        lowered.diagnostics
    );
    lowered
        .program
        .register_package(std_package.clone(), [], false);
    lowered
        .program
        .register_package(root_package, [(Name::new("std"), std_package)], true);
    let analysis = analyze(&lowered.program);
    (lowered.program, analysis)
}

fn analyze_source(source: &str) -> Vec<loom_core::Diagnostic> {
    analyze_source_program(source).1.diagnostics
}

fn definition_named(program: &Program, package: &PackageId, module: &str, name: &str) -> DefId {
    program
        .definitions
        .iter()
        .find_map(|(definition, item)| {
            let owner = &program.modules[item.module];
            (owner.package == *package
                && owner.name.as_str() == module
                && item
                    .name
                    .as_ref()
                    .is_some_and(|candidate| candidate.as_str() == name))
            .then_some(definition)
        })
        .unwrap_or_else(|| panic!("missing {package:?} {module}.{name}"))
}

fn assert_builtin_error_result_types(
    program: &Program,
    analysis: &Analysis,
    owner: DefId,
    decode_error: DefId,
    path_error: DefId,
) {
    let DefinitionKind::Function(function) = &program.definitions[owner].kind else {
        panic!("builtin result owner must be a function");
    };
    let body = analysis
        .typed
        .body(function.body)
        .expect("builtin result owner must have checked semantics");
    let mut decode_results = 0_usize;
    let mut path_results = 0_usize;
    for (expression, call) in body.calls.iter() {
        let expected = match call.target {
            CallTarget::Builtin(BuiltinValue::BytesDecodeUtf8) => {
                decode_results = decode_results.saturating_add(1);
                decode_error
            }
            CallTarget::Builtin(BuiltinValue::PathFromText | BuiltinValue::PathJoin) => {
                path_results = path_results.saturating_add(1);
                path_error
            }
            _ => continue,
        };
        let result = *body
            .expression_types
            .get(expression)
            .expect("builtin call result type");
        let TyData::Result { error, .. } = analysis.typed.types.data(result) else {
            panic!("builtin error operation must return Result");
        };
        assert_eq!(
            analysis.typed.types.data(*error),
            &TyData::Nominal {
                definition: expected,
                arguments: Vec::new(),
            },
            "builtin result must use the exact compiler-owned source enum"
        );
    }
    assert_eq!(decode_results, 1);
    assert_eq!(path_results, 2);
}

#[test]
fn text_bytes_path_and_path_file_calls_type_check() {
    let diagnostics = analyze_source(
        r#"
import std.file.open_read_path
import std.file.create_path
import std.path.PathError
import std.text.DecodeTextError

concept IndexSelf {
    method indexSelf(self) TextMap[Self]
}

record Token {}

impl IndexSelf for Token {
    method indexSelf(self) TextMap[Token] {
        TextMap[Token]().insert("token", self)
    }
}

fn values(text Text, bytes Bytes, base Path, child Path, index Int) {
    let scalar_count = text.length()
    let scalar = text.get(index)
    let concatenated = text.concat("!")
    let contained = text.contains("loom")
    let encoded = text.encode_utf8()
    let rebuilt = Text.from_utf8_units([76, 111, 111, 109])
    let byte_count = bytes.length()
    let byte = bytes.get(index)
    let appended = bytes.append(encoded)
    let decoded = appended.decode_utf8()
    var output = text.encode_utf8()
    output.add(0)
    output.add(255)
    let rendered = base.as_text()
    let joined = base.join(child)
    let parsed = Path.from_text(text)
    assert bytes == bytes
    assert base == base
}

fn conceptValue(token Token) TextMap[Token] {
    token.indexSelf()
}

fn decodeOutcome(value Result[Text, DecodeTextError]) {
    match value {
        Ok(_) => Unit
        Err(error) => match error {
            DecodeTextError.InvalidUtf8 => Unit
        }
    }
}

fn pathOutcome(value Result[Path, PathError]) {
    match value {
        Ok(_) => Unit
        Err(error) => match error {
            PathError.ContainsNul => Unit
            PathError.AbsoluteJoin => Unit
        }
    }
}

async fn pathFiles(path Path) {
    scoped input = open_read_path(path).await
    scoped output = create_path(path).await
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn bytes_add_requires_a_mutable_receiver_and_an_int_value() {
    let diagnostics = analyze_source(
        r#"
fn wrong(text Text) {
    let immutable = text.encode_utf8()
    immutable.add(1)
    text.encode_utf8().add(2)
    var mutable = text.encode_utf8()
    mutable.add("not a byte")
}
"#,
    );
    let codes = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        codes
            .iter()
            .filter(|code| **code == "MutReceiverRequiresVar")
            .count(),
        2,
        "{diagnostics:#?}"
    );
    assert!(codes.contains(&"TypeMismatch"), "{diagnostics:#?}");
}

#[test]
fn builtin_error_results_and_patterns_use_exact_source_definitions() {
    let (program, analysis) = analyze_source_program(
        r"
import std.path.PathError
import std.text.DecodeTextError

fn values(bytes Bytes, text Text, base Path, child Path) {
    let decoded = bytes.decode_utf8()
    let parsed = Path.from_text(text)
    let joined = base.join(child)
}

fn decodeOutcome(value Result[Text, DecodeTextError]) {
    match value {
        Ok(_) => Unit
        Err(error) => match error {
            DecodeTextError.InvalidUtf8 => Unit
        }
    }
}

fn pathOutcome(value Result[Path, PathError]) {
    match value {
        Ok(_) => Unit
        Err(error) => match error {
            PathError.ContainsNul => Unit
            PathError.AbsoluteJoin => Unit
        }
    }
}
",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );

    let std = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let application = PackageId::new("sema-test", "0");
    let decode_error = definition_named(&program, &std, "std.text", "DecodeTextError");
    let path_error = definition_named(&program, &std, "std.path", "PathError");
    let values = definition_named(&program, &application, "sema_test", "values");
    assert_eq!(
        analysis.canonical_std_items.decode_text_error,
        Some(decode_error)
    );
    assert_eq!(analysis.canonical_std_items.path_error, Some(path_error));

    let DefinitionKind::Enum(decode_enum) = &program.definitions[decode_error].kind else {
        panic!("DecodeTextError must be an ordinary source enum");
    };
    let DefinitionKind::Enum(path_enum) = &program.definitions[path_error].kind else {
        panic!("PathError must be an ordinary source enum");
    };
    let source_variants = decode_enum
        .variants
        .iter()
        .chain(&path_enum.variants)
        .copied()
        .collect::<Vec<_>>();
    let resolved_patterns = analysis
        .typed
        .bodies
        .values()
        .flat_map(|body| body.pattern_resolutions.values())
        .filter_map(|resolution| match resolution {
            Resolution::Definition(definition) if source_variants.contains(definition) => {
                Some(*definition)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for variant in &source_variants {
        assert!(
            resolved_patterns.contains(variant),
            "source variant {variant:?} was not selected by a pattern: {resolved_patterns:#?}"
        );
    }

    assert_builtin_error_result_types(&program, &analysis, values, decode_error, path_error);
}

#[test]
fn enum_pattern_qualifiers_must_resolve_to_the_expected_source_type() {
    let diagnostics = analyze_source(
        r"
import std.path.PathError
import std.text.DecodeTextError

enum FakeDecodeError {
    InvalidUtf8
}

enum FakePathError {
    ContainsNul
    AbsoluteJoin
}

fn decodeOutcome(value DecodeTextError) {
    match value {
        FakeDecodeError.InvalidUtf8 => Unit
        DecodeTextError.InvalidUtf8 => Unit
    }
}

fn pathOutcome(value PathError) {
    match value {
        FakePathError.ContainsNul => Unit
        PathError.ContainsNul => Unit
        PathError.AbsoluteJoin => Unit
    }
}
",
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "UnknownName")
            .count(),
        2,
        "a same-shaped enum must not qualify variants of the expected source type: {diagnostics:#?}"
    );
}

#[test]
fn same_named_application_errors_cannot_replace_canonical_std_results() {
    let diagnostics = analyze_source(
        r"
enum DecodeTextError {
    InvalidUtf8
}

enum PathError {
    ContainsNul
    AbsoluteJoin
}

fn decode(bytes Bytes) Result[Text, DecodeTextError] {
    bytes.decode_utf8()
}

fn path(text Text) Result[Path, PathError] {
    Path.from_text(text)
}
",
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "TypeMismatch")
            .count(),
        2,
        "same-named application enums must not gain canonical authority: {diagnostics:#?}"
    );
}

#[test]
fn text_from_utf8_units_requires_exactly_one_int_list() {
    let diagnostics = analyze_source(
        r#"
fn wrong() {
    let textList = Text.from_utf8_units(["not", "bytes"])
    let floatList = Text.from_utf8_units([65.0])
    let extraArgument = Text.from_utf8_units([65], [66])
}
"#,
    );
    let codes = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.iter().filter(|code| **code == "TypeMismatch").count() >= 4,
        "{diagnostics:#?}"
    );
}

#[test]
fn builtin_value_calls_reject_wrong_shapes_and_incomplete_error_matches() {
    let diagnostics = analyze_source(
        r#"
import std.path.PathError

fn wrong(text Text, bytes Bytes, path Path) {
    let scalar = text.get("zero")
    let appended = bytes.append(text)
    let parsed = Path.from_text(1)
    let joined = path.join(text)
}

fn incomplete(error PathError) {
    match error {
        PathError.ContainsNul => Unit
    }
}
"#,
    );
    let codes = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.iter().filter(|code| **code == "TypeMismatch").count() >= 4,
        "{diagnostics:#?}"
    );
    assert!(codes.contains(&"NonExhaustiveMatch"), "{diagnostics:#?}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn text_map_json_typed_io_and_logging_type_check() {
    let (program, analysis) = analyze_source_program(
        r#"
import std.file.try_open_read
import std.file.try_create
import std.file.try_open_read_path
import std.file.try_create_path
import std.net.try_connect
import std.json.parse_json
import std.json.format_json
import std.log.debug
import std.log.info
import std.log.warn
import std.log.error
import std.log.LogLevel
import std.log.write

fn values(text Text) {
    let empty = TextMap[Text]()
    let inserted = empty.insert("name", text)
    let overwritten = inserted.insert("name", "loom")
    let removed = overwritten.remove("absent")
    let count = removed.length()
    let present = removed.contains("name")
    let value = removed.get("name")
    let first = removed.entry_at(0)
    assert removed == removed

    let null = Json.Null
    let boolean = Json.Bool(true)
    let number = Json.Number(1.5)
    let string = Json.Text(text)
    let array = Json.Array([null, boolean])
    let object = Json.Object(TextMap[Json]().insert("answer", number))
    let parsed = parse_json("{\"answer\":42}")
    let formatted = format_json(object)
    let syntax = JsonError.InvalidSyntax(2)
    let depth = JsonError.DepthLimit
    let level = LogLevel.Info
    debug("debug")
    info("info")
    warn("warn")
    error("error")
    write(level, "event", removed)
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

fn logLevel(level LogLevel) {
    match level {
        LogLevel.Debug => Unit
        LogLevel.Info => Unit
        LogLevel.Warn => Unit
        LogLevel.Error => Unit
    }
}

async fn files(path Path) Result[Unit, IoError] {
    scoped input = try_open_read_path(path).await?
    let content = input.try_read_text().await?
    scoped output = try_create_path(path).await?
    output.try_write_text(content).await?
    Ok(Unit)
}

async fn textFiles(path Text) Result[Unit, IoError] {
    scoped input = try_open_read(path).await?
    scoped output = try_create(path).await?
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
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );

    let std = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let application = PackageId::new("sema-test", "0");
    let format_json = definition_named(&program, &std, "std.json", "format_json");
    let values = definition_named(&program, &application, "sema_test", "values");
    let DefinitionKind::Function(values) = &program.definitions[values].kind else {
        panic!("values must be a function")
    };
    let values_body = analysis
        .typed
        .body(values.body)
        .expect("checked values body");
    assert!(
        values_body.calls.values().any(
            |call| matches!(call.target, CallTarget::Function(target) if target == format_json)
        ),
        "format_json must resolve to its ordinary std source definition: {:#?}",
        values_body.calls
    );
}

#[test]
fn list_tuple_bulk_text_map_constructor_has_one_exact_generic_shape() {
    let diagnostics = analyze_source(
        r"
fn valid(entries List[(Text, Int)]) Result[TextMap[Int], Text] {
    entries.to_text_map()
}
",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let diagnostics = analyze_source(
        r"
fn wrong(values List[Int], wrongKeys List[(Int, Int)], entries List[(Text, Int)]) {
    let notPairs = values.to_text_map()
    let notTextKeys = wrongKeys.to_text_map()
    let extraArgument = entries.to_text_map(1)
}
",
    );
    assert!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "TypeMismatch")
            .count()
            >= 3,
        "{diagnostics:#?}"
    );
}

#[test]
fn structured_builtin_values_reject_wrong_shapes_and_open_matches() {
    let diagnostics = analyze_source(
        r#"
import std.json.format_json
import std.log.LogLevel
import std.log.write

fn genericEquality[T](left TextMap[T], right TextMap[T]) Bool {
    left == right
}

fn wrong(text Text) {
    let map = TextMap[Int]().insert(1, text)
    let entry = map.entry_at("zero")
    let missing = Json.Null()
    let badJson = Json.Bool(text)
    let formatted = format_json(text)
    write(LogLevel.Info, "event", TextMap[Int]())
}

fn incompleteJson(value Json) {
    match value {
        Null => Unit
        Bool(_) => Unit
    }
}

fn incompleteIo(kind IoErrorKind) {
    match kind {
        Other => Unit
    }
}
"#,
    );
    let codes = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"InvalidGenericOperation"),
        "{diagnostics:#?}"
    );
    assert!(
        codes.iter().filter(|code| **code == "TypeMismatch").count() >= 4,
        "{diagnostics:#?}"
    );
    assert!(
        codes
            .iter()
            .filter(|code| **code == "NonExhaustiveMatch")
            .count()
            >= 2,
        "{diagnostics:#?}"
    );
}

#[test]
fn wrapped_resources_must_be_unwrapped_directly_into_scoped() {
    let diagnostics = analyze_source(
        r"
import std.file.try_open_read

fn consume[T](value T) {
}

async fn stored(path Text) {
    let outcome = try_open_read(path).await
}

async fn discarded(path Text) {
    try_open_read(path).await
}

async fn wildcard(path Text) {
    match try_open_read(path).await {
        Ok(_) => Unit
        Err(_) => Unit
    }
}

async fn extractedButNotScoped(path Text) {
    match try_open_read(path).await {
        Ok(file) => {
            let alias = file
            Unit
        }
        Err(_) => Unit
    }
}

async fn usedAfterScopedTransfer(path Text) {
    match try_open_read(path).await {
        Ok(file) => {
            scoped resource = file
            let duplicate = file
            Unit
        }
        Err(_) => Unit
    }
}

async fn passed(path Text) {
    consume(try_open_read(path).await)
}

async fn aggregate() {
    let files = List[File]()
}
",
    );
    let codes = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert!(
        codes
            .iter()
            .filter(|code| **code == "MustScopeRequiresScoped")
            .count()
            >= 5,
        "{diagnostics:#?}"
    );
    assert!(
        codes.contains(&"MustScopeArgumentNotAllowed"),
        "{diagnostics:#?}"
    );
    assert!(
        codes.contains(&"MustScopeAlreadyTransferred"),
        "{diagnostics:#?}"
    );
}

#[test]
fn task_wrapped_resources_can_wait_then_enter_scoped() {
    let diagnostics = analyze_source(
        r"
import std.file.try_open_read

async fn direct(path Text) Result[Unit, IoError] {
    let pending = try_open_read(path)
    scoped file = pending.await?
    let read = file.try_read_text().await?
    Ok(Unit)
}

async fn matched(path Text) {
    let pending = try_open_read(path)
    match pending.await {
        Ok(file) => {
            scoped file = file
            let read = file.try_read_text().await
            Unit
        }
        Err(_) => Unit
    }
}
",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}
