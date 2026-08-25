use loom_core::FileId;
use loom_hir::{SourceUnit, lower_files};
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fn analyze_source(source: &str) -> Vec<loom_core::Diagnostic> {
    let parsed = parse_with_file(FileId(0), source);
    assert!(
        parsed.diagnostics().is_empty(),
        "syntax diagnostics: {:#?}",
        parsed.diagnostics()
    );
    let lowered = lower_files([SourceUnit {
        file: FileId(0),
        syntax: parsed.ast(),
    }]);
    assert!(
        lowered.diagnostics.is_empty(),
        "HIR diagnostics: {:#?}",
        lowered.diagnostics
    );
    analyze(&lowered.program).diagnostics
}

#[test]
fn text_bytes_path_and_path_file_calls_type_check() {
    let diagnostics = analyze_source(
        r#"
module standard.resource

import standard.file.open_read_path
import standard.file.create_path

concept Dispose {
    method dispose(mut self) Unit
}

concept MustScope {}
concept NoSuspend {}

fn values(text Text, bytes Bytes, base Path, child Path, index Int) Unit {
    let scalar_count = text.length()
    let scalar = text.get(index)
    let concatenated = text.concat("!")
    let contained = text.contains("loom")
    let encoded = text.encode_utf8()
    let byte_count = bytes.length()
    let byte = bytes.get(index)
    let appended = bytes.append(encoded)
    let decoded = appended.decode_utf8()
    let rendered = base.as_text()
    let joined = base.join(child)
    let parsed = Path.from_text(text)
    assert bytes == bytes
    assert base == base
    Unit
}

fn decodeOutcome(value Result[Text, DecodeTextError]) Unit {
    match value {
        Ok(_) => Unit
        Err(error) => match error {
            InvalidUtf8 => Unit
        }
    }
}

fn pathOutcome(value Result[Path, PathError]) Unit {
    match value {
        Ok(_) => Unit
        Err(error) => match error {
            ContainsNul => Unit
            AbsoluteJoin => Unit
        }
    }
}

async fn pathFiles(path Path) Unit {
    scoped input = open_read_path(path).await
    scoped output = create_path(path).await
    Unit
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn standard_value_calls_reject_wrong_shapes_and_incomplete_error_matches() {
    let diagnostics = analyze_source(
        r#"
module sample

fn wrong(text Text, bytes Bytes, path Path) Unit {
    let scalar = text.get("zero")
    let appended = bytes.append(text)
    let parsed = Path.from_text(1)
    let joined = path.join(text)
    Unit
}

fn incomplete(error PathError) Unit {
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
