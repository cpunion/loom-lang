use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use loom_core::{Diagnostic, FileId, Severity};
use loom_driver::{AnalysisHost, AnalysisSnapshot, Position, SourceMap, is_valid_identifier};
use serde_json::{Value, json};

const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const SERVER_NOT_INITIALIZED: i64 = -32002;
const PIPELINE_INCOMPLETE: i64 = -32004;

/// Runs the language server over stdin/stdout using LSP's Content-Length
/// framing.
///
/// # Errors
///
/// Returns an I/O error when framed input cannot be read or output cannot be
/// written.
pub fn run_stdio() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run(BufReader::new(stdin.lock()), stdout.lock())
}

/// Runs one synchronous LSP session. The implementation is intentionally
/// single-threaded so every response and notification observes one driver
/// snapshot at a time.
///
/// # Errors
///
/// Returns an I/O error when a message cannot be read, decoded, or written.
pub fn run(reader: impl BufRead, writer: impl Write) -> io::Result<()> {
    Server::new(reader, writer).run()
}

struct Server<R, W> {
    reader: R,
    writer: W,
    host: Option<AnalysisHost>,
    open_documents: BTreeMap<String, PathBuf>,
    published_uris: BTreeSet<String>,
    shutdown_requested: bool,
}

impl<R: BufRead, W: Write> Server<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            host: None,
            open_documents: BTreeMap::new(),
            published_uris: BTreeSet::new(),
            shutdown_requested: false,
        }
    }

    fn run(&mut self) -> io::Result<()> {
        while let Some(message) = read_message(&mut self.reader)? {
            if self.handle(message)? {
                break;
            }
        }
        Ok(())
    }

    /// Returns true when the session should terminate.
    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    fn handle(&mut self, message: Value) -> io::Result<bool> {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            if let Some(id) = message.get("id").cloned() {
                self.respond_error(id, INVALID_REQUEST, "request has no method", None)?;
            }
            return Ok(false);
        };
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        if self.shutdown_requested && method != "exit" {
            if let Some(id) = id {
                self.respond_error(id, INVALID_REQUEST, "server is shutting down", None)?;
            }
            return Ok(false);
        }
        match method {
            "initialize" => {
                let Some(id) = id else {
                    return Ok(false);
                };
                match initialize_root(&params).and_then(AnalysisHost::new) {
                    Ok(host) => {
                        self.host = Some(host);
                        self.respond(
                            id,
                            json!({
                                "capabilities": {
                                    "positionEncoding": "utf-16",
                                    "textDocumentSync": {"openClose": true, "change": 1},
                                    "diagnosticProvider": {
                                        "identifier": "loom",
                                        "interFileDependencies": true,
                                        "workspaceDiagnostics": false
                                    },
                                    "definitionProvider": true,
                                    "referencesProvider": true,
                                    "renameProvider": {"prepareProvider": true},
                                    "hoverProvider": true,
                                    "completionProvider": {"triggerCharacters": ["."]},
                                    "documentSymbolProvider": true,
                                    "workspaceSymbolProvider": true
                                },
                                "serverInfo": {
                                    "name": "loom-lsp",
                                    "version": env!("CARGO_PKG_VERSION")
                                },
                                "experimental": {
                                    "loomSemanticReferences": "project-and-callable-locals"
                                }
                            }),
                        )?;
                    }
                    Err(error) => {
                        self.respond_error(
                            id,
                            INVALID_PARAMS,
                            &format!("cannot open project: {error}"),
                            None,
                        )?;
                    }
                }
            }
            "initialized" => {}
            "shutdown" => {
                if let Some(id) = id {
                    self.shutdown_requested = true;
                    self.respond(id, Value::Null)?;
                }
            }
            "exit" => return Ok(true),
            "textDocument/didOpen" => self.did_open(&params)?,
            "textDocument/didChange" => self.did_change(&params)?,
            "textDocument/didClose" => self.did_close(&params)?,
            "textDocument/diagnostic" => {
                if let Some(id) = id {
                    self.document_diagnostic(id, &params)?;
                }
            }
            "textDocument/definition" => {
                if let Some(id) = id {
                    self.definition(id, &params)?;
                }
            }
            "textDocument/hover" => {
                if let Some(id) = id {
                    self.hover(id, &params)?;
                }
            }
            "textDocument/references" => {
                if let Some(id) = id {
                    self.references(id, &params)?;
                }
            }
            "textDocument/completion" => {
                if let Some(id) = id {
                    self.completion(id, &params)?;
                }
            }
            "textDocument/documentSymbol" => {
                if let Some(id) = id {
                    self.document_symbols(id, &params)?;
                }
            }
            "workspace/symbol" => {
                if let Some(id) = id {
                    self.workspace_symbols(id, &params)?;
                }
            }
            "textDocument/prepareRename" => {
                if let Some(id) = id {
                    self.prepare_rename(id, &params)?;
                }
            }
            "textDocument/rename" => {
                if let Some(id) = id {
                    self.rename(id, &params)?;
                }
            }
            _ => {
                if let Some(id) = id {
                    self.respond_error(
                        id,
                        METHOD_NOT_FOUND,
                        &format!("unsupported method `{method}`"),
                        None,
                    )?;
                }
            }
        }
        Ok(false)
    }

    fn did_open(&mut self, params: &Value) -> io::Result<()> {
        let Some(uri) = pointer_str(params, "/textDocument/uri") else {
            return self.log_error("didOpen is missing textDocument.uri");
        };
        let Some(text) = pointer_str(params, "/textDocument/text") else {
            return self.log_error("didOpen is missing textDocument.text");
        };
        let path = match file_uri_to_path(uri) {
            Ok(path) => path,
            Err(error) => return self.log_error(&error),
        };
        let Some(host) = self.host.as_mut() else {
            return self.log_error("server has not been initialized");
        };
        let path = match host.resolve_path(&path) {
            Ok(path) => path,
            Err(error) => return self.log_error(&error.to_string()),
        };
        host.set_overlay(&path, text)
            .expect("a resolved project path remains inside the project");
        self.open_documents.insert(uri.to_owned(), path);
        self.publish_diagnostics()
    }

    fn did_change(&mut self, params: &Value) -> io::Result<()> {
        let Some(uri) = pointer_str(params, "/textDocument/uri") else {
            return self.log_error("didChange is missing textDocument.uri");
        };
        let Some(changes) = params.get("contentChanges").and_then(Value::as_array) else {
            return self.log_error("didChange is missing contentChanges");
        };
        let Some(change) = changes.last() else {
            return Ok(());
        };
        if change.get("range").is_some() {
            return self.log_error("loom-lsp negotiated full document synchronization");
        }
        let Some(text) = change.get("text").and_then(Value::as_str) else {
            return self.log_error("didChange content has no text");
        };
        let path = self
            .open_documents
            .get(uri)
            .cloned()
            .or_else(|| file_uri_to_path(uri).ok());
        let Some(path) = path else {
            return self.log_error("didChange URI is not a valid file URI");
        };
        let Some(host) = self.host.as_mut() else {
            return self.log_error("server has not been initialized");
        };
        if let Err(error) = host.set_overlay(path, text) {
            return self.log_error(&error.to_string());
        }
        self.publish_diagnostics()
    }

    fn did_close(&mut self, params: &Value) -> io::Result<()> {
        let Some(uri) = pointer_str(params, "/textDocument/uri") else {
            return self.log_error("didClose is missing textDocument.uri");
        };
        if let Some(path) = self.open_documents.remove(uri)
            && let Some(host) = self.host.as_mut()
            && let Err(error) = host.clear_overlay(path)
        {
            self.log_error(&error.to_string())?;
        }
        self.publish_diagnostics()
    }

    fn document_diagnostic(&mut self, id: Value, params: &Value) -> io::Result<()> {
        let Some(uri) = pointer_str(params, "/textDocument/uri") else {
            return self.respond_error(id, INVALID_PARAMS, "missing textDocument.uri", None);
        };
        let Some(snapshot) = self.snapshot_or_error(id.clone())? else {
            return Ok(());
        };
        let items = diagnostics_for_uri(&snapshot, uri);
        self.respond(id, json!({"kind": "full", "items": items}))
    }

    fn definition(&mut self, id: Value, params: &Value) -> io::Result<()> {
        let Some((uri, position)) = text_position(params) else {
            return self.respond_error(id, INVALID_PARAMS, "missing text document position", None);
        };
        let Some(snapshot) = self.snapshot_or_error(id.clone())? else {
            return Ok(());
        };
        let Some((file, byte)) = file_and_byte(&snapshot, uri, position) else {
            return self.respond(id, Value::Null);
        };
        let Some(symbol) = snapshot.definition_at(file, byte) else {
            return self.respond(id, Value::Null);
        };
        let Some(source) = snapshot.sources().document(symbol.definition.file) else {
            return self.respond(id, Value::Null);
        };
        if !source.is_root_package() {
            return self.respond_error(
                id,
                INVALID_REQUEST,
                "dependency sources are read-only",
                Some(json!({"code": "DependencySourceReadOnly"})),
            );
        }
        self.respond(
            id,
            json!({
                "uri": self.uri_for_path(source.absolute_path()),
                "range": source.utf16_range(
                    symbol.definition.range.start,
                    symbol.definition.range.end
                )
            }),
        )
    }

    fn hover(&mut self, id: Value, params: &Value) -> io::Result<()> {
        let Some((uri, position)) = text_position(params) else {
            return self.respond_error(id, INVALID_PARAMS, "missing text document position", None);
        };
        let Some(snapshot) = self.snapshot_or_error(id.clone())? else {
            return Ok(());
        };
        let Some((file, byte)) = file_and_byte(&snapshot, uri, position) else {
            return self.respond(id, Value::Null);
        };
        let Some(symbol) = snapshot.definition_at(file, byte) else {
            return self.respond(id, Value::Null);
        };
        self.respond(
            id,
            json!({
                "contents": {
                    "kind": "markdown",
                    "value": format!("`{} {}`  \nmodule `{}`", symbol.kind, symbol.name, symbol.module)
                }
            }),
        )
    }

    fn references(&mut self, id: Value, params: &Value) -> io::Result<()> {
        let Some((uri, position)) = text_position(params) else {
            return self.respond_error(id, INVALID_PARAMS, "missing text document position", None);
        };
        let include_declaration = params
            .pointer("/context/includeDeclaration")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let Some(snapshot) = self.snapshot_or_error(id.clone())? else {
            return Ok(());
        };
        if snapshot.has_errors() {
            return self.incomplete_index(id);
        }
        let Some((file, byte)) = file_and_byte(&snapshot, uri, position) else {
            return self.respond(id, json!([]));
        };
        let Some(references) = snapshot.references_at(file, byte, include_declaration) else {
            return self.respond(id, json!([]));
        };
        let locations = references
            .into_iter()
            .filter_map(|reference| {
                let source = snapshot.sources().document(reference.span.file)?;
                Some(json!({
                    "uri": self.uri_for_path(source.absolute_path()),
                    "range": source.utf16_range(
                        reference.span.range.start,
                        reference.span.range.end
                    )
                }))
            })
            .collect::<Vec<_>>();
        self.respond(id, json!(locations))
    }

    fn completion(&mut self, id: Value, params: &Value) -> io::Result<()> {
        let Some((uri, position)) = text_position(params) else {
            return self.respond_error(id, INVALID_PARAMS, "missing text document position", None);
        };
        let Some(snapshot) = self.snapshot_or_error(id.clone())? else {
            return Ok(());
        };
        let Some((file, byte)) = file_and_byte(&snapshot, uri, position) else {
            return self.respond(id, json!({"isIncomplete": false, "items": []}));
        };
        let mut items = snapshot
            .completion_symbols(file, byte)
            .into_iter()
            .map(|symbol| {
                json!({
                    "label": symbol.name,
                    "kind": completion_kind(symbol.kind),
                    "detail": format!("{} · {}", symbol.kind, symbol.module),
                    "insertText": symbol.name,
                    "sortText": format!("0-{}-{}", symbol.name, symbol.module)
                })
            })
            .collect::<Vec<_>>();
        items.extend(COMPLETION_KEYWORDS.iter().map(|keyword| {
            json!({
                "label": keyword,
                "kind": 14,
                "detail": "Loom keyword",
                "sortText": format!("1-{keyword}")
            })
        }));
        items.sort_by(|left, right| {
            left.pointer("/sortText")
                .and_then(Value::as_str)
                .cmp(&right.pointer("/sortText").and_then(Value::as_str))
        });
        items.dedup_by(|left, right| {
            left.get("label") == right.get("label") && left.get("detail") == right.get("detail")
        });
        self.respond(id, json!({"isIncomplete": false, "items": items}))
    }

    fn document_symbols(&mut self, id: Value, params: &Value) -> io::Result<()> {
        let Some(uri) = pointer_str(params, "/textDocument/uri") else {
            return self.respond_error(id, INVALID_PARAMS, "missing textDocument.uri", None);
        };
        let Some(snapshot) = self.snapshot_or_error(id.clone())? else {
            return Ok(());
        };
        let Ok(path) = file_uri_to_path(uri) else {
            return self.respond(id, json!([]));
        };
        let Some(file) = snapshot.sources().file_id(&path) else {
            return self.respond(id, json!([]));
        };
        let Some(source) = snapshot.sources().document(file) else {
            return self.respond(id, json!([]));
        };
        let symbols = snapshot
            .document_symbols(file)
            .into_iter()
            .map(|symbol| {
                let range =
                    source.utf16_range(symbol.definition.range.start, symbol.definition.range.end);
                json!({
                    "name": symbol.name,
                    "detail": format!("{} · {}", symbol.kind, symbol.module),
                    "kind": symbol_kind(symbol.kind),
                    "range": range,
                    "selectionRange": range
                })
            })
            .collect::<Vec<_>>();
        self.respond(id, json!(symbols))
    }

    fn workspace_symbols(&mut self, id: Value, params: &Value) -> io::Result<()> {
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let Some(snapshot) = self.snapshot_or_error(id.clone())? else {
            return Ok(());
        };
        let symbols = snapshot
            .symbols()
            .into_iter()
            .filter(|symbol| {
                query.is_empty()
                    || symbol.name.to_lowercase().contains(&query)
                    || symbol.module.to_lowercase().contains(&query)
            })
            .filter_map(|symbol| {
                let source = snapshot.sources().document(symbol.definition.file)?;
                Some(json!({
                    "name": symbol.name,
                    "kind": symbol_kind(symbol.kind),
                    "location": {
                        "uri": self.uri_for_path(source.absolute_path()),
                        "range": source.utf16_range(
                            symbol.definition.range.start,
                            symbol.definition.range.end
                        )
                    },
                    "containerName": symbol.module
                }))
            })
            .collect::<Vec<_>>();
        self.respond(id, json!(symbols))
    }

    fn prepare_rename(&mut self, id: Value, params: &Value) -> io::Result<()> {
        let Some((uri, position)) = text_position(params) else {
            return self.respond_error(id, INVALID_PARAMS, "missing text document position", None);
        };
        let Some(snapshot) = self.snapshot_or_error(id.clone())? else {
            return Ok(());
        };
        if snapshot.has_errors() {
            return self.incomplete_index(id);
        }
        let Some((file, byte)) = file_and_byte(&snapshot, uri, position) else {
            return self.respond(id, Value::Null);
        };
        let Some(symbol) = snapshot.definition_at(file, byte) else {
            return self.respond(id, Value::Null);
        };
        let Some(source) = snapshot.sources().document(symbol.definition.file) else {
            return self.respond(id, Value::Null);
        };
        self.respond(
            id,
            json!({
                "range": source.utf16_range(
                    symbol.definition.range.start,
                    symbol.definition.range.end
                ),
                "placeholder": symbol.name
            }),
        )
    }

    fn rename(&mut self, id: Value, params: &Value) -> io::Result<()> {
        let Some((uri, position)) = text_position(params) else {
            return self.respond_error(id, INVALID_PARAMS, "missing text document position", None);
        };
        let Some(new_name) = params.get("newName").and_then(Value::as_str) else {
            return self.respond_error(id, INVALID_PARAMS, "missing newName", None);
        };
        if !is_valid_identifier(new_name) {
            return self.respond_error(
                id,
                INVALID_PARAMS,
                "newName must be one non-keyword Unicode XID identifier",
                None,
            );
        }
        let Some(snapshot) = self.snapshot_or_error(id.clone())? else {
            return Ok(());
        };
        if snapshot.has_errors() {
            return self.incomplete_index(id);
        }
        let Some((file, byte)) = file_and_byte(&snapshot, uri, position) else {
            return self.respond(id, Value::Null);
        };
        let Some(references) = snapshot.references_at(file, byte, true) else {
            return self.respond(id, Value::Null);
        };
        let definition_file = references
            .iter()
            .find(|reference| reference.is_declaration)
            .map(|reference| reference.span.file);
        if definition_file
            .and_then(|file| snapshot.sources().document(file))
            .is_some_and(|source| !source.is_root_package())
        {
            return self.respond_error(
                id,
                INVALID_REQUEST,
                "dependency sources are read-only",
                Some(json!({"code": "DependencySourceReadOnly"})),
            );
        }
        let mut changes = serde_json::Map::new();
        for reference in references {
            let Some(source) = snapshot.sources().document(reference.span.file) else {
                continue;
            };
            let uri = self.uri_for_path(source.absolute_path());
            let edits = changes.entry(uri).or_insert_with(|| json!([]));
            let Some(edits) = edits.as_array_mut() else {
                continue;
            };
            edits.push(json!({
                "range": source.utf16_range(
                    reference.span.range.start,
                    reference.span.range.end
                ),
                "newText": new_name
            }));
        }
        self.respond(id, json!({"changes": changes}))
    }

    fn incomplete_index(&mut self, id: Value) -> io::Result<()> {
        self.respond_error(
            id,
            PIPELINE_INCOMPLETE,
            "semantic references require a snapshot without source errors",
            Some(json!({"code": "CompilerPipelineIncomplete"})),
        )
    }

    fn publish_diagnostics(&mut self) -> io::Result<()> {
        let snapshot = match self.host.as_ref().map(AnalysisHost::snapshot) {
            Some(Ok(snapshot)) => snapshot,
            Some(Err(error)) => return self.log_error(&error.to_string()),
            None => return self.log_error("server has not been initialized"),
        };
        let mut current = BTreeSet::new();
        for source in snapshot.sources().documents() {
            let uri = self.uri_for_path(source.absolute_path());
            current.insert(uri.clone());
            self.notify(
                "textDocument/publishDiagnostics",
                json!({
                    "uri": uri,
                    "diagnostics": diagnostics_for_file(&snapshot, source.id())
                }),
            )?;
        }
        let stale = self
            .published_uris
            .difference(&current)
            .cloned()
            .collect::<Vec<_>>();
        for uri in stale {
            self.notify(
                "textDocument/publishDiagnostics",
                json!({"uri": uri, "diagnostics": []}),
            )?;
        }
        self.published_uris = current;
        Ok(())
    }

    fn uri_for_path(&self, path: &Path) -> String {
        self.open_documents
            .iter()
            .find_map(|(uri, open_path)| (open_path == path).then(|| uri.clone()))
            .unwrap_or_else(|| path_to_file_uri(path))
    }

    fn snapshot_or_error(&mut self, id: Value) -> io::Result<Option<AnalysisSnapshot>> {
        let Some(host) = self.host.as_ref() else {
            self.respond_error(
                id,
                SERVER_NOT_INITIALIZED,
                "server has not been initialized",
                None,
            )?;
            return Ok(None);
        };
        match host.snapshot() {
            Ok(snapshot) => Ok(Some(snapshot)),
            Err(error) => {
                self.respond_error(
                    id,
                    INVALID_REQUEST,
                    &format!("cannot build project snapshot: {error}"),
                    None,
                )?;
                Ok(None)
            }
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn respond(&mut self, id: Value, result: Value) -> io::Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    #[allow(clippy::needless_pass_by_value)]
    fn respond_error(
        &mut self,
        id: Value,
        code: i64,
        message: &str,
        data: Option<Value>,
    ) -> io::Result<()> {
        let mut error = json!({"code": code, "message": message});
        if let Some(data) = data {
            error["data"] = data;
        }
        self.send(&json!({"jsonrpc": "2.0", "id": id, "error": error}))
    }

    #[allow(clippy::needless_pass_by_value)]
    fn notify(&mut self, method: &str, params: Value) -> io::Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn log_error(&mut self, message: &str) -> io::Result<()> {
        self.notify("window/logMessage", json!({"type": 1, "message": message}))
    }

    fn send(&mut self, value: &Value) -> io::Result<()> {
        let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
        write!(self.writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
        self.writer.write_all(&bytes)?;
        self.writer.flush()
    }
}

const COMPLETION_KEYWORDS: &[&str] = &[
    "async",
    "assert",
    "concept",
    "defer",
    "dyn",
    "else",
    "enum",
    "ensures",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "import",
    "invariant",
    "let",
    "match",
    "module",
    "pub",
    "record",
    "requires",
    "return",
    "scoped",
    "test",
    "true",
    "var",
    "where",
];

fn symbol_kind(kind: &str) -> u8 {
    match kind {
        "function" | "test" => 12,
        "method" => 6,
        "record" | "constrained type" => 23,
        "enum" => 10,
        "enum variant" => 22,
        "concept" | "conformance" | "impl" => 11,
        "field" => 8,
        "associated type" | "type parameter" => 26,
        _ => 13,
    }
}

fn completion_kind(kind: &str) -> u8 {
    match kind {
        "function" | "test" => 3,
        "method" => 2,
        "record" | "constrained type" => 22,
        "enum" => 13,
        "enum variant" => 20,
        "concept" | "conformance" | "impl" => 8,
        "field" => 5,
        "associated type" | "type parameter" => 25,
        _ => 6,
    }
}

fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("Content-Length")
        {
            content_length = Some(value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length")
            })?);
        }
    }
    let length = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(io::Error::other)
}

fn initialize_root(params: &Value) -> Result<PathBuf, loom_driver::DriverError> {
    if let Some(uri) = params.get("rootUri").and_then(Value::as_str) {
        return file_uri_to_path(uri)
            .map_err(|message| loom_driver::DriverError::InvalidRoot(message.into()));
    }
    if let Some(path) = params.get("rootPath").and_then(Value::as_str) {
        return Ok(PathBuf::from(path));
    }
    std::env::current_dir().map_err(|source| loom_driver::DriverError::Io {
        path: PathBuf::from("."),
        source,
    })
}

fn diagnostics_for_uri(snapshot: &AnalysisSnapshot, uri: &str) -> Vec<Value> {
    let Ok(path) = file_uri_to_path(uri) else {
        return Vec::new();
    };
    let Some(file) = snapshot.sources().file_id(&path) else {
        return Vec::new();
    };
    diagnostics_for_file(snapshot, file)
}

fn diagnostics_for_file(snapshot: &AnalysisSnapshot, file: FileId) -> Vec<Value> {
    snapshot
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.primary.file == file)
        .filter_map(|diagnostic| diagnostic_to_lsp(diagnostic, snapshot.sources()))
        .collect()
}

fn diagnostic_to_lsp(diagnostic: &Diagnostic, sources: &SourceMap) -> Option<Value> {
    let source = sources.document(diagnostic.primary.file)?;
    let related = diagnostic
        .labels
        .iter()
        .filter_map(|label| {
            let source = sources.document(label.span.file)?;
            Some(json!({
                "location": {
                    "uri": path_to_file_uri(source.absolute_path()),
                    "range": source.utf16_range(label.span.range.start, label.span.range.end)
                },
                "message": label.message
            }))
        })
        .collect::<Vec<_>>();
    Some(json!({
        "range": source.utf16_range(
            diagnostic.primary.range.start,
            diagnostic.primary.range.end
        ),
        "severity": match diagnostic.severity {
            Severity::Error => 1,
            Severity::Warning => 2,
            Severity::Information => 3,
        },
        "code": diagnostic.code,
        "source": "loom",
        "message": diagnostic.message,
        "relatedInformation": related,
        "data": {"notes": diagnostic.notes}
    }))
}

fn text_position(params: &Value) -> Option<(&str, Position)> {
    Some((
        pointer_str(params, "/textDocument/uri")?,
        Position {
            line: u32::try_from(params.pointer("/position/line")?.as_u64()?).ok()?,
            character: u32::try_from(params.pointer("/position/character")?.as_u64()?).ok()?,
        },
    ))
}

fn file_and_byte(
    snapshot: &AnalysisSnapshot,
    uri: &str,
    position: Position,
) -> Option<(FileId, u32)> {
    let path = file_uri_to_path(uri).ok()?;
    let file = snapshot.sources().file_id(&path)?;
    let byte = snapshot
        .sources()
        .document(file)?
        .byte_offset_utf16(position)?;
    Some((file, byte))
}

fn pointer_str<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer)?.as_str()
}

/// Converts an absolute local path to a percent-encoded file URI.
#[must_use]
pub fn path_to_file_uri(path: &Path) -> String {
    let path = path.to_string_lossy();
    let mut uri = String::from("file://");
    for byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            uri.push(char::from(*byte));
        } else {
            uri.push('%');
            let _ = write!(uri, "{byte:02X}");
        }
    }
    uri
}

/// Decodes a local `file://` URI. Remote authorities are rejected.
///
/// # Errors
///
/// Returns a descriptive message for an unsupported authority, malformed
/// percent escape, or non-UTF-8 decoded path.
pub fn file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| "only file:// URIs are supported".to_owned())?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return Err("remote file URI authorities are not supported".to_owned());
    }
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' {
            if index + 2 >= raw.len() {
                return Err("truncated percent escape in file URI".to_owned());
            }
            let high = hex(raw[index + 1])?;
            let low = hex(raw[index + 2])?;
            bytes.push((high << 4) | low);
            index += 3;
        } else {
            bytes.push(raw[index]);
            index += 1;
        }
    }
    String::from_utf8(bytes)
        .map(PathBuf::from)
        .map_err(|_| "file URI path is not valid UTF-8".to_owned())
}

fn hex(value: u8) -> Result<u8, String> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err("invalid percent escape in file URI".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{file_uri_to_path, path_to_file_uri};

    #[test]
    fn file_uri_round_trip_handles_spaces_and_unicode() {
        let path = Path::new("/tmp/loom project/价格.loom");
        let uri = path_to_file_uri(path);
        assert_eq!(file_uri_to_path(&uri).as_deref(), Ok(path));
    }
}
