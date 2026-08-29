use loom_core::{FileId, LOOM_LANGUAGE_VERSION, Name, PackageId};
use loom_hir::{PackageSourceUnit, lower_package_files};
use loom_lowering::lower_to_mir;
use loom_sema::analyze;
use loom_syntax::parse_with_file;

pub const STRUCTURED_STANDARD_SOURCE: &str = r#"
module fuzz.structured_standard

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

pub fn main() {
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
    let standard_int = parse_with_file(FileId(1), include_str!("../../library/std/src/int.loom"));
    let standard_log = parse_with_file(FileId(2), include_str!("../../library/std/src/log.loom"));
    let standard_resource = parse_with_file(
        FileId(3),
        include_str!("../../library/std/src/resource.loom"),
    );
    if !parsed.diagnostics().is_empty() {
        return Err(format!("syntax diagnostics: {:#?}", parsed.diagnostics()));
    }
    if !standard_int.diagnostics().is_empty()
        || !standard_log.diagnostics().is_empty()
        || !standard_resource.diagnostics().is_empty()
    {
        return Err(format!(
            "standard-library syntax diagnostics: int={:#?}, log={:#?}, resource={:#?}",
            standard_int.diagnostics(),
            standard_log.diagnostics(),
            standard_resource.diagnostics()
        ));
    }
    let root_package = PackageId::new("fuzz", "0");
    let standard_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: FileId(0),
            package: root_package.clone(),
            syntax: parsed.ast(),
        },
        PackageSourceUnit {
            file: FileId(1),
            package: standard_package.clone(),
            syntax: standard_int.ast(),
        },
        PackageSourceUnit {
            file: FileId(2),
            package: standard_package.clone(),
            syntax: standard_log.ast(),
        },
        PackageSourceUnit {
            file: FileId(3),
            package: standard_package.clone(),
            syntax: standard_resource.ast(),
        },
    ]);
    if !lowered.diagnostics.is_empty() {
        return Err(format!("HIR diagnostics: {:#?}", lowered.diagnostics));
    }
    lowered
        .program
        .register_package(standard_package.clone(), [], false);
    lowered.program.register_package(
        root_package,
        [(Name::new("std"), standard_package)],
        true,
    );
    let analysis = analyze(&lowered.program);
    if !analysis.diagnostics.is_empty() {
        return Err(format!("semantic diagnostics: {:#?}", analysis.diagnostics));
    }
    lower_to_mir(&lowered.program, &analysis)
        .map_err(|failure| format!("MIR lowering diagnostics: {:#?}", failure.diagnostics()))
}
