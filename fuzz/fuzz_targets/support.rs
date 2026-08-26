use loom_core::FileId;
use loom_hir::{SourceUnit, lower_files};
use loom_lowering::lower_to_mir;
use loom_sema::analyze;
use loom_syntax::parse_with_file;

pub const STRUCTURED_STANDARD_SOURCE: &str = r#"
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

pub fn main() Unit {
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
    Unit
}
"#;

pub fn compile(source: &str) -> Result<loom_mir::CheckedProgram, String> {
    let parsed = parse_with_file(FileId(0), source);
    if !parsed.diagnostics().is_empty() {
        return Err(format!("syntax diagnostics: {:#?}", parsed.diagnostics()));
    }
    let lowered = lower_files([SourceUnit {
        file: FileId(0),
        syntax: parsed.ast(),
    }]);
    if !lowered.diagnostics.is_empty() {
        return Err(format!("HIR diagnostics: {:#?}", lowered.diagnostics));
    }
    let analysis = analyze(&lowered.program);
    if !analysis.diagnostics.is_empty() {
        return Err(format!("semantic diagnostics: {:#?}", analysis.diagnostics));
    }
    lower_to_mir(&lowered.program, &analysis)
        .map_err(|failure| format!("MIR lowering diagnostics: {:#?}", failure.diagnostics()))
}
