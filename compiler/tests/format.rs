use std::{fs, io::Write, process::Stdio};

mod common;
use common::{command, loom, success};

#[test]
fn format_cli_preserves_sources_and_round_trips_utf8_input() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.loom");
    let source = path.to_str().unwrap();
    let original = "fn main(){assert true}";
    let formatted = "fn main() {\n    assert true\n}\n";
    fs::write(&path, original).unwrap();

    let check = loom(&["fmt", source, "--check"]);
    assert!(!check.status.success());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    let stdout = loom(&["fmt", source, "--stdout"]);
    success(&stdout);
    assert_eq!(stdout.stdout, formatted.as_bytes());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    success(&loom(&["fmt", source]));
    assert_eq!(fs::read_to_string(&path).unwrap(), formatted);
    success(&loom(&["fmt", source, "--check"]));
    success(&loom(&["fmt", source]));
    assert_eq!(fs::read_to_string(&path).unwrap(), formatted);

    let invalid = "fn main() { let x: Int = 1 }";
    fs::write(&path, invalid).unwrap();
    assert!(!loom(&["fmt", source]).status.success());
    assert_eq!(fs::read_to_string(&path).unwrap(), invalid);

    // Cross the stdin reader's chunk boundary inside multi-byte UTF-8 text.
    let input = format!(
        "fn greeting() Text {{\n    \"{}\"\n}}\n",
        "雪😀".repeat(2048)
    );
    assert!(input.len() > 8192);
    let mut child = command(&["fmt", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    success(&output);
    assert_eq!(output.stdout, input.as_bytes());
    assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
}
