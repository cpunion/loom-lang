use std::process::Command;

use loom_codegen_llvm::EmitOptions;
use loom_driver::AnalysisHost;

mod support;
use support::{emit_native, loom_text_literal};

#[test]
#[allow(clippy::too_many_lines)]
fn text_bytes_paths_and_path_files_match_in_both_backends() {
    let project = tempfile::tempdir().expect("create standard-value project");
    let file = loom_text_literal(
        project
            .path()
            .join("path-round-trip.txt")
            .to_str()
            .expect("temporary path is UTF-8"),
    );
    let source = format!(
        r#"module builtin_values

import std.file.open_read_path
import std.file.create_path

test fn text_bytes_and_paths() {{
    let text = "a界🙂"
    let scalar_count = text.length()
    assert scalar_count == 3
    match text.get(1) {{
        Some(value) => {{
            assert value == "界"
            Unit
        }}
        None => {{
            assert false
            Unit
        }}
    }}
    let concatenated = text.concat("!")
    assert concatenated == "a界🙂!"
    let contained = text.contains("界🙂")
    assert contained

    let bytes = text.encode_utf8()
    let byte_count = bytes.length()
    assert byte_count == 8
    match bytes.get(1) {{
        Some(value) => {{
            assert value == 231
            Unit
        }}
        None => {{
            assert false
            Unit
        }}
    }}
    let appended = bytes.append("!".encode_utf8())
    match appended.decode_utf8() {{
        Ok(value) => {{
            assert value == "a界🙂!"
            Unit
        }}
        Err(InvalidUtf8) => {{
            assert false
            Unit
        }}
    }}

    match Path.from_text("root") {{
        Ok(base) => match Path.from_text("child/file") {{
            Ok(child) => {{
                match base.join(child) {{
                    Ok(value) => {{
                        let rendered = value.as_text()
                        assert rendered == "root/child/file"
                        Unit
                    }}
                    Err(ContainsNul) => {{
                        assert false
                        Unit
                    }}
                    Err(AbsoluteJoin) => {{
                        assert false
                        Unit
                    }}
                }}
                match Path.from_text("/absolute") {{
                    Ok(absolute) => match base.join(absolute) {{
                        Err(AbsoluteJoin) => Unit
                        Err(ContainsNul) => {{
                            assert false
                            Unit
                        }}
                        Ok(_) => {{
                            assert false
                            Unit
                        }}
                    }}
                    Err(_) => {{
                        assert false
                        Unit
                    }}
                }}
                Unit
            }}
            Err(_) => {{
                assert false
                Unit
            }}
        }}
        Err(_) => {{
            assert false
            Unit
        }}
    }}
}}

test async fn path_file_round_trip() {{
    match Path.from_text("{file}") {{
        Ok(file) => {{
            {{
                scoped output = create_path(file).await
                output.write_text("path I/O").await
                Unit
            }}
            {{
                scoped input = open_read_path(file).await
                let content = input.read_text().await
                assert content == "path I/O"
                Unit
            }}
            Unit
        }}
        Err(_) => {{
            assert false
            Unit
        }}
    }}
}}
"#,
    );
    std::fs::write(project.path().join("main.loom"), source).expect("write source");

    let snapshot = AnalysisHost::new(project.path())
        .expect("load standard-value project")
        .snapshot()
        .expect("analyze standard-value project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());

    let interpreted = snapshot.run_tests().expect("run interpreter tests");
    assert_eq!(interpreted.len(), 2, "{interpreted:#?}");
    assert!(
        interpreted.iter().all(|result| result.failure.is_none()),
        "{interpreted:#?}"
    );

    let executable = project.path().join("native-tests");
    emit_native(
        snapshot.executable().expect("lower standard-value MIR"),
        &executable,
        &EmitOptions::tests(),
    )
    .expect("emit standard-value native tests");
    let output = Command::new(executable)
        .output()
        .expect("run standard-value native tests");
    assert!(
        output.status.success(),
        "status={:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("native output is UTF-8");
    assert!(
        stdout.contains("passed builtin_values.text_bytes_and_paths\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("passed builtin_values.path_file_round_trip\n"),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("path-round-trip.txt")).unwrap(),
        "path I/O"
    );
}
