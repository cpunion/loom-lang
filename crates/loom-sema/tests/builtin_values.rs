use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{PackageSourceUnit, lower_package_files};
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fn analyze_source(source: &str) -> Vec<loom_core::Diagnostic> {
    let root_file = FileId(0);
    let std_log_file = FileId(1);
    let std_json_file = FileId(2);
    let std_float_file = FileId(3);
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
    assert!(
        parsed.diagnostics().is_empty(),
        "syntax diagnostics: {:#?}",
        parsed.diagnostics()
    );
    assert!(
        std_log.diagnostics().is_empty()
            && std_json.diagnostics().is_empty()
            && std_float.diagnostics().is_empty(),
        "standard syntax diagnostics: log={:#?} json={:#?} float={:#?}",
        std_log.diagnostics(),
        std_json.diagnostics(),
        std_float.diagnostics()
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
    analyze(&lowered.program).diagnostics
}

#[test]
fn text_bytes_path_and_path_file_calls_type_check() {
    let diagnostics = analyze_source(
        r#"
import std.file.open_read_path
import std.file.create_path

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
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
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
fn wrong(text Text, bytes Bytes, path Path) {
    let scalar = text.get("zero")
    let appended = bytes.append(text)
    let parsed = Path.from_text(1)
    let joined = path.join(text)
}

fn incomplete(error PathError) {
    match error {
        ContainsNul => Unit
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
    let diagnostics = analyze_source(
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
        Debug => Unit
        Info => Unit
        Warn => Unit
        Error => Unit
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
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
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
