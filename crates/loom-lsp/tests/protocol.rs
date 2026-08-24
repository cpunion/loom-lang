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
fn stdio_session_shares_overlay_diagnostics_and_refuses_incomplete_rename_index() {
    let source = "module demo\n\nfn main() Unit {\n    Unit\n}\n";
    let project = TestProject::new(source);
    let root_uri = loom_lsp::path_to_file_uri(&project.0);
    let file_uri = loom_lsp::path_to_file_uri(&project.0.join("main.loom"));
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":file_uri,"languageId":"loom","version":1,"text":source}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/definition","params":{"textDocument":{"uri":file_uri},"position":{"line":2,"character":4}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/references","params":{"textDocument":{"uri":file_uri},"position":{"line":2,"character":4},"context":{"includeDeclaration":true}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"textDocument/rename","params":{"textDocument":{"uri":file_uri},"position":{"line":2,"character":4},"newName":"renamed"}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":file_uri,"version":2},"contentChanges":[{"text":"module demo\n\nfn broken("}]}}),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":null}),
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

    let definition = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(2)))
        .expect("definition response");
    assert_eq!(definition.pointer("/result/uri"), Some(&json!(file_uri)));

    let references = responses
        .iter()
        .find(|message| message.get("id") == Some(&json!(3)))
        .expect("references response");
    assert_eq!(
        references
            .pointer("/result")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
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
                    .filter(|edit| edit.get("newText") == Some(&json!("renamed")))
                    .count()
            }),
        Some(1)
    );

    assert!(responses.iter().any(|message| {
        message.get("method") == Some(&json!("textDocument/publishDiagnostics"))
            && message
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .is_some_and(|diagnostics| !diagnostics.is_empty())
    }));
}
