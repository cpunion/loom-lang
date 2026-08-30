use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{PackageSourceUnit, lower_package_files};
use loom_lowering::lower_to_mir;
use loom_sema::analyze;
use loom_syntax::parse_with_file;

pub const STRUCTURED_BUILTIN_SOURCE: &str = r#"
import std.file.try_open_read_path
import std.file.try_create_path
import std.net.try_connect
import std.json.parse_json
import std.json.format_json
import std.int.ParseIntError
import std.int.parse_int
import std.log.debug
import std.log.info
import std.log.warn
import std.log.error
import std.log.write

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

fn parsedInteger(text Text) Int {
    match parse_int(text) {
        Ok(value) => value
        Err(ParseIntError.InvalidSyntax) => -1
        Err(ParseIntError.OutOfRange) => -2
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

pub fn main() {
    let integer = parsedInteger("42")
    let invalidInteger = parsedInteger("not-an-integer")
    let outOfRangeInteger = parsedInteger("9223372036854775808")
    assert integer == 42
    assert invalidInteger == -1
    assert outOfRangeInteger == -2
    let fields = TextMap[Text]().insert("key", "value").remove("missing")
    let present = fields.contains("key")
    let found = fields.get("key")
    let document = Json.Object(TextMap[Json]().insert("answer", Json.Number(42.0)))
    let parsed = parse_json("null")
    let formatted = format_json(document)
    debug("debug")
    info("info")
    warn("warn")
    error("error")
    write(LogLevel.Info, "event", fields)
}
"#;

pub fn compile(source: &str) -> Result<loom_mir::CheckedProgram, String> {
    let parsed = parse_with_file(FileId(0), source);
    let std_int = parse_with_file(FileId(1), include_str!("../../library/std/int/int.loom"));
    let std_log = parse_with_file(FileId(2), include_str!("../../library/std/log/log.loom"));
    let std_resource = parse_with_file(
        FileId(3),
        include_str!("../../library/std/resource/resource.loom"),
    );
    let std_json = parse_with_file(FileId(4), include_str!("../../library/std/json/json.loom"));
    let std_io = parse_with_file(FileId(5), include_str!("../../library/std/io/io.loom"));
    let std_file = parse_with_file(FileId(6), include_str!("../../library/std/file/file.loom"));
    let std_net = parse_with_file(FileId(7), include_str!("../../library/std/net/net.loom"));
    if !parsed.diagnostics().is_empty() {
        return Err(format!("syntax diagnostics: {:#?}", parsed.diagnostics()));
    }
    if !std_int.diagnostics().is_empty()
        || !std_log.diagnostics().is_empty()
        || !std_resource.diagnostics().is_empty()
        || !std_json.diagnostics().is_empty()
        || !std_io.diagnostics().is_empty()
        || !std_file.diagnostics().is_empty()
        || !std_net.diagnostics().is_empty()
    {
        return Err(format!(
            "std source syntax diagnostics: int={:#?}, json={:#?}, log={:#?}, resource={:#?}, io={:#?}, file={:#?}, net={:#?}",
            std_int.diagnostics(),
            std_json.diagnostics(),
            std_log.diagnostics(),
            std_resource.diagnostics(),
            std_io.diagnostics(),
            std_file.diagnostics(),
            std_net.diagnostics()
        ));
    }
    let root_package = PackageId::new("fuzz", "0");
    let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: FileId(0),
            package: root_package.clone(),
            module: ModuleName::new("fuzz"),
            syntax: parsed.ast(),
        },
        PackageSourceUnit {
            file: FileId(1),
            package: std_package.clone(),
            module: ModuleName::new("std.int"),
            syntax: std_int.ast(),
        },
        PackageSourceUnit {
            file: FileId(2),
            package: std_package.clone(),
            module: ModuleName::new("std.log"),
            syntax: std_log.ast(),
        },
        PackageSourceUnit {
            file: FileId(3),
            package: std_package.clone(),
            module: ModuleName::new("std.resource"),
            syntax: std_resource.ast(),
        },
        PackageSourceUnit {
            file: FileId(4),
            package: std_package.clone(),
            module: ModuleName::new("std.json"),
            syntax: std_json.ast(),
        },
        PackageSourceUnit {
            file: FileId(5),
            package: std_package.clone(),
            module: ModuleName::new("std.io"),
            syntax: std_io.ast(),
        },
        PackageSourceUnit {
            file: FileId(6),
            package: std_package.clone(),
            module: ModuleName::new("std.file"),
            syntax: std_file.ast(),
        },
        PackageSourceUnit {
            file: FileId(7),
            package: std_package.clone(),
            module: ModuleName::new("std.net"),
            syntax: std_net.ast(),
        },
    ]);
    if !lowered.diagnostics.is_empty() {
        return Err(format!("HIR diagnostics: {:#?}", lowered.diagnostics));
    }
    lowered
        .program
        .register_package(std_package.clone(), [], false);
    lowered
        .program
        .register_package(root_package, [(Name::new("std"), std_package)], true);
    let analysis = analyze(&lowered.program);
    if !analysis.diagnostics.is_empty() {
        return Err(format!("semantic diagnostics: {:#?}", analysis.diagnostics));
    }
    lower_to_mir(&lowered.program, &analysis)
        .map_err(|failure| format!("MIR lowering diagnostics: {:#?}", failure.diagnostics()))
}
