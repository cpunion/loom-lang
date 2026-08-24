use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

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

#[test]
#[allow(clippy::too_many_lines)]
fn stdio_session_exposes_semantic_navigation_and_refuses_an_incomplete_index() {
    let source = "module demo\n\npub fn identity[T](value T) T {\n    let copy = value\n    copy\n}\n\npub fn main() Unit {\n    let answer = identity(42)\n    assert answer == 42\n    Unit\n}\n";
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
