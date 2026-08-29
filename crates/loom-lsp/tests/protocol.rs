use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use loom_driver::{AnalysisHost, encode_library_artifact};
use serde_json::{Value, json};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestProject(PathBuf);

impl TestProject {
    fn new(source: &str) -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("loom-lsp-test-{}-{serial}", std::process::id()));
        fs::create_dir_all(&root).expect("create project");
        fs::write(root.join("main.loom"), source).expect("write source");
        Self(root)
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().expect("test path has parent"))
            .expect("create source parent");
        fs::write(path, text).expect("write source");
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn frame(message: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).expect("serialize request");
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend(body);
    framed
}

fn decode_frames(mut bytes: &[u8]) -> Vec<Value> {
    let mut messages = Vec::new();
    while !bytes.is_empty() {
        let header_end = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("complete LSP header");
        let header = std::str::from_utf8(&bytes[..header_end]).expect("UTF-8 header");
        let length = header
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .expect("Content-Length")
            .parse::<usize>()
            .expect("numeric length");
        bytes = &bytes[header_end + 4..];
        messages.push(serde_json::from_slice(&bytes[..length]).expect("JSON response"));
        bytes = &bytes[length..];
    }
    messages
}

fn read_frame(reader: &mut impl BufRead) -> Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read LSP header");
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = Some(value.trim().parse::<usize>().expect("numeric length"));
        }
    }
    let mut body = vec![0; length.expect("Content-Length")];
    reader.read_exact(&mut body).expect("read LSP body");
    serde_json::from_slice(&body).expect("JSON response")
}

fn read_until_id(reader: &mut impl BufRead, id: i64) -> (Value, Vec<Value>) {
    let mut preceding = Vec::new();
    loop {
        let message = read_frame(reader);
        if message.get("id") == Some(&json!(id)) {
            return (message, preceding);
        }
        preceding.push(message);
    }
}

fn source_position(source: &str, needle: &str) -> Value {
    let byte = source
        .find(needle)
        .unwrap_or_else(|| panic!("missing {needle}"));
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let character = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, line)| line)
        .encode_utf16()
        .count();
    json!({"line": line, "character": character})
}

#[test]
fn structured_standard_values_are_discoverable_through_completion_and_hover() {
    let source = r#"module editor.standard

import std.json.parse_json
import std.log.write

fn inspect(problem IoError, value Json) {
    let fields = TextMap[Text]().insert("key", "value")
    let parsed = parse_json("null")
    write(LogLevel.Info, problem.message(), fields)
}
"#;
    let project = TestProject::new(source);
    let root_uri = loom_lsp::path_to_file_uri(&project.0);
    let file_uri = loom_lsp::path_to_file_uri(&project.0.join("main.loom"));
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":file_uri,"languageId":"loom","version":1,"text":source}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":file_uri},"position":source_position(source, "TextMap")}}),
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{"textDocument":{"uri":file_uri},"position":source_position(source, "parse_json")}}),
        json!({"jsonrpc":"2.0","id":4,"method":"textDocument/hover","params":{"textDocument":{"uri":file_uri},"position":source_position(source, "insert")}}),
        json!({"jsonrpc":"2.0","id":5,"method":"textDocument/hover","params":{"textDocument":{"uri":file_uri},"position":source_position(source, "IoError")}}),
        json!({"jsonrpc":"2.0","id":6,"method":"textDocument/completion","params":{"textDocument":{"uri":file_uri},"position":source_position(source, "\n}")}}),
        json!({"jsonrpc":"2.0","id":7,"method":"shutdown","params":null}),
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    ];
    let input = messages.iter().flat_map(frame).collect::<Vec<_>>();
    let mut output = Vec::new();
    loom_lsp::run(BufReader::new(input.as_slice()), &mut output).expect("run LSP session");
    let responses = decode_frames(&output);

    for (id, signature) in [
        (2, "TextMap[V]"),
        (3, "fn parse_json(text Text) Result[Json, JsonError]"),
        (
            4,
            "method insert[V](self TextMap[V], key Text, value V) TextMap[V]",
        ),
        (5, "record IoError"),
    ] {
        let hover = responses
            .iter()
            .find(|message| message.get("id") == Some(&json!(id)))
            .unwrap_or_else(|| panic!("missing hover response {id}"));
        let markdown = hover
            .pointer("/result/contents/value")
            .and_then(Value::as_str)
            .expect("hover markdown");
        assert!(markdown.contains(signature), "{markdown}");
    }

    let labels = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(6)))
        .and_then(|message| message.pointer("/result/items"))
        .and_then(Value::as_array)
        .expect("completion items")
        .iter()
        .filter_map(|item| item.get("label").and_then(Value::as_str))
        .collect::<Vec<_>>();
    for expected in [
        "TextMap",
        "Json",
        "JsonError",
        "IoError",
        "IoErrorKind",
        "entry_at",
        "LogLevel",
        "parse_json",
        "format_json",
        "write",
        "insert",
        "remove",
        "kind",
        "message",
        "try_open_read_path",
        "try_connect",
        "try_read_text",
        "discard",
    ] {
        assert!(labels.contains(&expected), "missing {expected}: {labels:?}");
    }
}

#[test]
fn compiler_owned_standard_sources_report_distinct_navigation_and_mutation_policy() {
    let source = r"module editor.compiler_owned

import std.int.minimum
import std.resource.MustScope

record ResourceMarker {}

impl MustScope for ResourceMarker {}

pub fn main() {
    let selected = minimum(2, 1)
    assert selected == 1
}
";
    let project = TestProject::new(source);
    let root_uri = loom_lsp::path_to_file_uri(&project.0);
    let file_uri = loom_lsp::path_to_file_uri(&project.0.join("main.loom"));
    let minimum_position = source_position(source, "minimum(2");
    let must_scope_position = source_position(source, "MustScope for");
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":file_uri,"languageId":"loom","version":1,"text":source}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/definition","params":{"textDocument":{"uri":file_uri},"position":minimum_position}}),
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/prepareRename","params":{"textDocument":{"uri":file_uri},"position":minimum_position}}),
        json!({"jsonrpc":"2.0","id":4,"method":"textDocument/rename","params":{"textDocument":{"uri":file_uri},"position":minimum_position,"newName":"smaller"}}),
        json!({"jsonrpc":"2.0","id":5,"method":"workspace/symbol","params":{"query":"minimum"}}),
        json!({"jsonrpc":"2.0","id":6,"method":"textDocument/definition","params":{"textDocument":{"uri":file_uri},"position":must_scope_position}}),
        json!({"jsonrpc":"2.0","id":7,"method":"textDocument/prepareRename","params":{"textDocument":{"uri":file_uri},"position":must_scope_position}}),
        json!({"jsonrpc":"2.0","id":8,"method":"textDocument/rename","params":{"textDocument":{"uri":file_uri},"position":must_scope_position,"newName":"OptionalScope"}}),
        json!({"jsonrpc":"2.0","id":9,"method":"workspace/symbol","params":{"query":"MustScope"}}),
        json!({"jsonrpc":"2.0","id":10,"method":"shutdown","params":null}),
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    ];
    let input = messages.iter().flat_map(frame).collect::<Vec<_>>();
    let mut output = Vec::new();
    loom_lsp::run(BufReader::new(input.as_slice()), &mut output).expect("run LSP session");
    let responses = decode_frames(&output);

    for (id, symbol) in [(2, "minimum"), (6, "MustScope")] {
        let definition = responses
            .iter()
            .find(|message| message.get("id") == Some(&json!(id)))
            .unwrap_or_else(|| panic!("missing {symbol} definition response"));
        assert_eq!(
            definition.pointer("/error/data/code"),
            Some(&json!("CompilerOwnedSourceNotNavigable")),
            "{symbol}: {definition:#?}"
        );
        assert_eq!(
            definition.pointer("/error/message"),
            Some(&json!(
                "compiler-owned standard library source is not a workspace document"
            )),
            "{symbol}: {definition:#?}"
        );
    }
    for id in [3, 4, 7, 8] {
        let response = responses
            .iter()
            .find(|message| message.get("id") == Some(&json!(id)))
            .unwrap_or_else(|| panic!("missing response {id}"));
        assert_eq!(
            response.pointer("/error/data/code"),
            Some(&json!("CompilerOwnedSourceReadOnly")),
            "{response:#?}"
        );
        assert_eq!(
            response.pointer("/error/message"),
            Some(&json!(
                "compiler-owned standard library sources are read-only"
            )),
            "{response:#?}"
        );
    }
    for (id, query) in [(5, "minimum"), (9, "MustScope")] {
        let symbols = responses
            .iter()
            .find(|message| message.get("id") == Some(&json!(id)))
            .and_then(|message| message.get("result"))
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("missing {query} workspace symbol response"));
        assert!(symbols.is_empty(), "{query}: {symbols:#?}");
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn stdio_session_exposes_semantic_navigation_and_refuses_an_incomplete_index() {
    let source = "module demo\n\npub fn identity[T](value T) T {\n    let copy = value\n    copy\n}\n\npub fn main() {\n    let answer = identity(42)\n    assert answer == 42  \n}\n";
    let project = TestProject::new(source);
    let root_uri = loom_lsp::path_to_file_uri(&project.0);
    let file_uri = loom_lsp::path_to_file_uri(&project.0.join("main.loom"));
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":file_uri,"languageId":"loom","version":1,"text":source}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/definition","params":{"textDocument":{"uri":file_uri},"position":{"line":8,"character":22}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/references","params":{"textDocument":{"uri":file_uri},"position":{"line":3,"character":16},"context":{"includeDeclaration":true}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"textDocument/prepareRename","params":{"textDocument":{"uri":file_uri},"position":{"line":4,"character":5}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"textDocument/rename","params":{"textDocument":{"uri":file_uri},"position":{"line":4,"character":5},"newName":"cloned"}}),
        json!({"jsonrpc":"2.0","id":6,"method":"textDocument/completion","params":{"textDocument":{"uri":file_uri},"position":{"line":4,"character":4}}}),
        json!({"jsonrpc":"2.0","id":7,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":file_uri}}}),
        json!({"jsonrpc":"2.0","id":8,"method":"workspace/symbol","params":{"query":"identity"}}),
        json!({"jsonrpc":"2.0","id":11,"method":"textDocument/formatting","params":{"textDocument":{"uri":file_uri},"options":{"tabSize":4,"insertSpaces":true}}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":file_uri,"version":2},"contentChanges":[{"text":"module demo\n\nfn broken("}]}}),
        json!({"jsonrpc":"2.0","id":9,"method":"textDocument/rename","params":{"textDocument":{"uri":file_uri},"position":{"line":0,"character":1},"newName":"renamed"}}),
        json!({"jsonrpc":"2.0","id":10,"method":"shutdown","params":null}),
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    ];

    let mut child = Command::new(env!("CARGO_BIN_EXE_loom-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loom-lsp");
    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        for message in messages {
            stdin.write_all(&frame(&message)).expect("write request");
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for loom-lsp");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses = decode_frames(&output.stdout);

    let initialize = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(1)))
        .expect("initialize response");
    assert_eq!(
        initialize.pointer("/result/capabilities/positionEncoding"),
        Some(&json!("utf-16"))
    );
    assert_eq!(
        initialize.pointer("/result/capabilities/renameProvider/prepareProvider"),
        Some(&json!(true))
    );
    assert_eq!(
        initialize.pointer("/result/capabilities/documentSymbolProvider"),
        Some(&json!(true))
    );
    assert_eq!(
        initialize.pointer("/result/capabilities/workspaceSymbolProvider"),
        Some(&json!(true))
    );
    assert_eq!(
        initialize.pointer("/result/capabilities/documentFormattingProvider"),
        Some(&json!(true))
    );

    let definition = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(2)))
        .expect("definition response");
    assert_eq!(definition.pointer("/result/uri"), Some(&json!(file_uri)));
    assert_eq!(
        definition.pointer("/result/range/start"),
        Some(&json!({"line": 2, "character": 7}))
    );

    let references = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(3)))
        .expect("references response");
    assert_eq!(
        references
            .pointer("/result")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(2)
    );

    let prepare_rename = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(4)))
        .expect("prepare rename response");
    assert_eq!(
        prepare_rename.pointer("/result/placeholder"),
        Some(&json!("copy"))
    );

    let rename = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(5)))
        .expect("rename response");
    assert_eq!(
        rename
            .pointer("/result/changes")
            .and_then(Value::as_object)
            .map(|changes| {
                changes
                    .values()
                    .flat_map(|edits| edits.as_array().into_iter().flatten())
                    .filter(|edit| edit.get("newText") == Some(&json!("cloned")))
                    .count()
            }),
        Some(2)
    );

    let completion = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(6)))
        .expect("completion response");
    let completion_labels = completion
        .pointer("/result/items")
        .and_then(Value::as_array)
        .expect("completion items")
        .iter()
        .filter_map(|item| item.get("label").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        ["identity", "T", "value", "copy", "let"]
            .iter()
            .all(|label| completion_labels.contains(label)),
        "{completion_labels:?}"
    );

    let document_symbols = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(7)))
        .and_then(|message| message.pointer("/result"))
        .and_then(Value::as_array)
        .expect("document symbols");
    let document_names = document_symbols
        .iter()
        .filter_map(|symbol| symbol.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        ["identity", "T", "value", "copy", "main", "answer"]
            .iter()
            .all(|name| document_names.contains(name)),
        "{document_names:?}"
    );

    let workspace_symbols = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(8)))
        .and_then(|message| message.pointer("/result"))
        .and_then(Value::as_array)
        .expect("workspace symbols");
    assert_eq!(workspace_symbols.len(), 1, "{workspace_symbols:#?}");
    assert_eq!(workspace_symbols[0].get("name"), Some(&json!("identity")));

    let formatting = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(11)))
        .and_then(|message| message.pointer("/result/0/newText"))
        .and_then(Value::as_str)
        .expect("full document formatting edit");
    assert!(!formatting.contains("42  \n"), "{formatting:?}");

    let refused_rename = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(9)))
        .expect("incomplete rename response");
    assert_eq!(refused_rename.pointer("/error/code"), Some(&json!(-32004)));

    assert!(responses.iter().any(|message| {
        message.get("method") == Some(&json!("textDocument/publishDiagnostics"))
            && message
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .is_some_and(|diagnostics| !diagnostics.is_empty())
    }));
}

#[test]
fn workspace_folders_are_independent_and_can_be_reloaded() {
    let alpha = TestProject::new("module alpha\n\npub fn apple() Int { 1 }\n");
    let beta = TestProject::new("module beta\n\npub fn banana() Int { 2 }\n");
    let alpha_uri = loom_lsp::path_to_file_uri(&alpha.0);
    let beta_uri = loom_lsp::path_to_file_uri(&beta.0);
    let watched_manifest = loom_lsp::path_to_file_uri(&alpha.0.join("loom.toml"));
    let watched_lock = loom_lsp::path_to_file_uri(&alpha.0.join("loom.lock"));
    let watched_artifact = loom_lsp::path_to_file_uri(&alpha.0.join("dependency.loomlib"));
    let messages = [
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "workspaceFolders":[
                    {"uri":alpha_uri,"name":"alpha"},
                    {"uri":beta_uri,"name":"beta"}
                ]
            }
        }),
        json!({"jsonrpc":"2.0","id":2,"method":"workspace/symbol","params":{"query":"a"}}),
        json!({
            "jsonrpc":"2.0",
            "method":"workspace/didChangeWorkspaceFolders",
            "params":{"event":{"added":[],"removed":[{"uri":beta_uri,"name":"beta"}]}}
        }),
        json!({"jsonrpc":"2.0","id":3,"method":"workspace/symbol","params":{"query":"banana"}}),
        json!({
            "jsonrpc":"2.0",
            "method":"workspace/didChangeWorkspaceFolders",
            "params":{"event":{"added":[{"uri":beta_uri,"name":"beta"}],"removed":[]}}
        }),
        json!({
            "jsonrpc":"2.0",
            "method":"workspace/didChangeWatchedFiles",
            "params":{"changes":[
                {"uri":watched_manifest,"type":2},
                {"uri":watched_lock,"type":2},
                {"uri":watched_artifact,"type":2}
            ]}
        }),
        json!({"jsonrpc":"2.0","id":4,"method":"workspace/symbol","params":{"query":"banana"}}),
        json!({"jsonrpc":"2.0","id":5,"method":"shutdown","params":null}),
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    ];

    let mut child = Command::new(env!("CARGO_BIN_EXE_loom-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loom-lsp");
    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        for message in messages {
            stdin.write_all(&frame(&message)).expect("write request");
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for loom-lsp");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses = decode_frames(&output.stdout);

    let initial = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(2)))
        .and_then(|message| message.get("result"))
        .and_then(Value::as_array)
        .expect("initial multi-root symbols");
    let names = initial
        .iter()
        .filter_map(|symbol| symbol.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(names.contains(&"apple"), "{initial:#?}");
    assert!(names.contains(&"banana"), "{initial:#?}");

    let removed = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(3)))
        .and_then(|message| message.get("result"))
        .and_then(Value::as_array)
        .expect("symbols after workspace removal");
    assert!(removed.is_empty(), "{removed:#?}");

    let restored = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(4)))
        .and_then(|message| message.get("result"))
        .and_then(Value::as_array)
        .expect("symbols after workspace reload");
    assert_eq!(restored.len(), 1, "{restored:#?}");
    assert_eq!(restored[0].get("name"), Some(&json!("banana")));
}

#[test]
fn manifest_and_lock_notifications_rebuild_the_project_graph() {
    let project = TestProject::new("module ignored\n");
    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"reload\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "src/main.loom",
        "module reload\n\npub fn old_value() Int { 1 }\n",
    );
    project.write(
        "generated/main.loom",
        "module reload\n\npub fn new_value() Int { 2 }\n",
    );
    let root_uri = loom_lsp::path_to_file_uri(&project.0);
    let manifest_uri = loom_lsp::path_to_file_uri(&project.0.join("loom.toml"));
    let lock_uri = loom_lsp::path_to_file_uri(&project.0.join("loom.lock"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_loom-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loom-lsp");
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    stdin
        .write_all(&frame(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri}}),
        ))
        .expect("initialize request");
    stdin.flush().expect("flush initialize");
    let _ = read_until_id(&mut stdout, 1);

    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"reload\"\nversion = \"1.0.0\"\nsources = [\"generated\"]\n",
    );
    stdin
        .write_all(&frame(&json!({"jsonrpc":"2.0","method":"workspace/didChangeWatchedFiles","params":{"changes":[{"uri":manifest_uri,"type":2}]}})))
        .expect("manifest notification");
    stdin
        .write_all(&frame(&json!({"jsonrpc":"2.0","id":2,"method":"workspace/symbol","params":{"query":"new_value"}})))
        .expect("workspace symbol request");
    stdin.flush().expect("flush manifest reload");
    let (reloaded, _) = read_until_id(&mut stdout, 2);
    let symbols = reloaded
        .get("result")
        .and_then(Value::as_array)
        .expect("symbols after manifest reload");
    assert_eq!(symbols.len(), 1, "{reloaded:#?}");
    assert_eq!(symbols[0].get("name"), Some(&json!("new_value")));

    project.write("loom.lock", "this is not valid TOML = [");
    stdin
        .write_all(&frame(&json!({"jsonrpc":"2.0","method":"workspace/didChangeWatchedFiles","params":{"changes":[{"uri":lock_uri,"type":2}]}})))
        .expect("lock notification");
    stdin
        .write_all(&frame(&json!({"jsonrpc":"2.0","id":3,"method":"workspace/symbol","params":{"query":"new_value"}})))
        .expect("delimiter request");
    stdin.flush().expect("flush lock reload");
    let (_, messages) = read_until_id(&mut stdout, 3);
    assert!(
        messages.iter().any(|message| {
            message.get("method") == Some(&json!("window/logMessage"))
                && message
                    .pointer("/params/message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains("cannot reload workspace"))
        }),
        "{messages:#?}"
    );

    fs::remove_file(project.0.join("loom.lock")).expect("remove invalid lock");
    stdin
        .write_all(&frame(&json!({"jsonrpc":"2.0","method":"workspace/didChangeWatchedFiles","params":{"changes":[{"uri":lock_uri,"type":3}]}})))
        .expect("lock removal notification");
    stdin
        .write_all(&frame(&json!({"jsonrpc":"2.0","id":4,"method":"workspace/symbol","params":{"query":"new_value"}})))
        .expect("recovered graph request");
    stdin.flush().expect("flush lock recovery");
    let (recovered, _) = read_until_id(&mut stdout, 4);
    assert_eq!(
        recovered
            .get("result")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "{recovered:#?}"
    );
    stdin
        .write_all(&frame(
            &json!({"jsonrpc":"2.0","id":5,"method":"shutdown","params":null}),
        ))
        .expect("shutdown request");
    stdin.flush().expect("flush shutdown");
    let _ = read_until_id(&mut stdout, 5);
    stdin
        .write_all(&frame(
            &json!({"jsonrpc":"2.0","method":"exit","params":null}),
        ))
        .expect("exit notification");
    drop(stdin);
    assert!(child.wait().expect("wait for loom-lsp").success());
}

#[test]
fn nested_workspace_documents_route_to_the_most_specific_root() {
    let project = TestProject::new("module ignored\n");
    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"outer\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "src/main.loom",
        "module outer\n\npub fn outer_value() Int { 1 }\n",
    );
    project.write(
        "nested/loom.toml",
        "schema = 1\n[package]\nname = \"inner\"\nversion = \"1.0.0\"\n",
    );
    let inner_source = "module inner\n\npub fn inner_value() Int { 2 }\n";
    project.write("nested/src/main.loom", inner_source);
    let outer_uri = loom_lsp::path_to_file_uri(&project.0);
    let inner_root_uri = loom_lsp::path_to_file_uri(&project.0.join("nested"));
    let inner_uri = loom_lsp::path_to_file_uri(&project.0.join("nested/src/main.loom"));
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"workspaceFolders":[{"uri":outer_uri,"name":"outer"},{"uri":inner_root_uri,"name":"inner"}]}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":inner_uri,"languageId":"loom","version":1,"text":inner_source}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":inner_uri}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":null}),
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    ];
    let mut child = Command::new(env!("CARGO_BIN_EXE_loom-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loom-lsp");
    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        for message in messages {
            stdin.write_all(&frame(&message)).expect("write request");
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for loom-lsp");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let symbols = decode_frames(&output.stdout)
        .into_iter()
        .find(|message| message.get("id") == Some(&json!(2)))
        .and_then(|message| message.get("result").cloned())
        .and_then(|result| result.as_array().cloned())
        .expect("inner document symbols");
    assert!(
        symbols
            .iter()
            .any(|symbol| symbol.get("name") == Some(&json!("inner_value"))),
        "{symbols:#?}"
    );
    assert!(
        symbols
            .iter()
            .all(|symbol| symbol.get("name") != Some(&json!("outer_value"))),
        "{symbols:#?}"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn portable_dependency_implementations_are_opaque_and_read_only() {
    let project = TestProject::new("module placeholder\n");
    project.write(
        "utility/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"utility\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "utility/src/lib.loom",
        "module utility\n\npub fn increment(value Int) Int { value + 1 }\n\nfn hidden() Int { 99 }\n",
    );
    let producer = AnalysisHost::new(project.0.join("utility")).expect("open producer");
    let snapshot = producer.snapshot().expect("compile producer");
    let artifact = encode_library_artifact(snapshot.project(), snapshot.sources())
        .expect("encode producer artifact");
    let artifact_path = project.0.join("utility.loomlib");
    fs::write(&artifact_path, &artifact).expect("write artifact");
    fs::remove_dir_all(project.0.join("utility")).expect("remove producer checkout");

    project.write(
        "consumer/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"consumer\"\nversion = \"1.0.0\"\n[dependencies]\nutility = { artifact = \"../utility.loomlib\", version = \"^1\" }\n",
    );
    let consumer_source = "module consumer\n\nimport utility.increment\n\npub fn main() {\n    let value = increment(41)\n    assert value == 42\n}\n";
    project.write("consumer/src/main.loom", consumer_source);
    let root_uri = loom_lsp::path_to_file_uri(&project.0.join("consumer"));
    let file_uri = loom_lsp::path_to_file_uri(&project.0.join("consumer/src/main.loom"));
    let artifact_uri = loom_lsp::path_to_file_uri(&artifact_path);
    let mut child = Command::new(env!("CARGO_BIN_EXE_loom-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loom-lsp");
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    stdin
        .write_all(&frame(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri}}),
        ))
        .expect("initialize request");
    stdin.flush().expect("flush initialize");
    let _ = read_until_id(&mut stdout, 1);

    fs::write(&artifact_path, b"corrupt portable library").expect("corrupt artifact");
    stdin
        .write_all(&frame(&json!({"jsonrpc":"2.0","method":"workspace/didChangeWatchedFiles","params":{"changes":[{"uri":artifact_uri,"type":2}]}})))
        .expect("artifact notification");
    stdin
        .write_all(&frame(&json!({"jsonrpc":"2.0","id":2,"method":"workspace/symbol","params":{"query":"increment"}})))
        .expect("delimiter request");
    stdin.flush().expect("flush corrupt artifact reload");
    let (_, reload_messages) = read_until_id(&mut stdout, 2);
    assert!(
        reload_messages.iter().any(|message| {
            message.get("method") == Some(&json!("window/logMessage"))
                && message
                    .pointer("/params/message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains("cannot reload workspace"))
        }),
        "{reload_messages:#?}"
    );

    fs::write(&artifact_path, &artifact).expect("restore artifact");
    for message in [
        json!({"jsonrpc":"2.0","method":"workspace/didChangeWatchedFiles","params":{"changes":[{"uri":artifact_uri,"type":2}]}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":file_uri,"languageId":"loom","version":1,"text":consumer_source}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/definition","params":{"textDocument":{"uri":file_uri},"position":{"line":5,"character":20}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"textDocument/prepareRename","params":{"textDocument":{"uri":file_uri},"position":{"line":5,"character":20}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"workspace/symbol","params":{"query":"increment"}}),
    ] {
        stdin.write_all(&frame(&message)).expect("write request");
    }
    stdin.flush().expect("flush restored artifact requests");
    let (definition, _) = read_until_id(&mut stdout, 3);
    assert_eq!(
        definition.pointer("/error/data/code"),
        Some(&json!("DependencyArtifactOpaque")),
        "{definition:#?}"
    );
    let (rename, _) = read_until_id(&mut stdout, 4);
    assert_eq!(
        rename.pointer("/error/data/code"),
        Some(&json!("DependencySourceReadOnly")),
        "{rename:#?}"
    );
    assert_eq!(
        rename.pointer("/error/message"),
        Some(&json!(
            "portable library implementation sources are read-only"
        )),
        "{rename:#?}"
    );
    let (symbols, _) = read_until_id(&mut stdout, 5);
    let symbols = symbols
        .get("result")
        .and_then(Value::as_array)
        .expect("workspace symbol response");
    assert!(symbols.is_empty(), "{symbols:#?}");
    stdin
        .write_all(&frame(
            &json!({"jsonrpc":"2.0","id":6,"method":"shutdown","params":null}),
        ))
        .expect("shutdown request");
    stdin.flush().expect("flush shutdown");
    let _ = read_until_id(&mut stdout, 6);
    stdin
        .write_all(&frame(
            &json!({"jsonrpc":"2.0","method":"exit","params":null}),
        ))
        .expect("exit notification");
    drop(stdin);
    assert!(child.wait().expect("wait for loom-lsp").success());
}
