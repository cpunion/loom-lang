use std::path::Path;
use std::process::Command;
use std::{io::Read as _, net::TcpListener};

use loom_codegen_llvm::{EmitOptions, NATIVE_RUNTIME_ABI, emit_native, native_runtime_identity};
use loom_driver::AnalysisHost;

const CHILD_PROJECT_ENV: &str = "LOOM_STDLIB_INTERPRETER_CHILD_PROJECT";
const EXPECTED_LOGS: &str = concat!(
    "{\"level\":\"debug\",\"message\":\"debug \\\"line\\\"\",\"fields\":{}}\n",
    "{\"level\":\"warn\",\"message\":\"event\\nline\",\"fields\":{\"a\":\"first\",\"z\":\"last\"}}\n",
);

fn snapshot(project: &Path) -> loom_driver::AnalysisSnapshot {
    AnalysisHost::new(project)
        .expect("load structured standard-library fixture")
        .snapshot()
        .expect("analyze structured standard-library fixture")
}

#[test]
fn interpreter_standard_library_child() {
    let Ok(project) = std::env::var(CHILD_PROJECT_ENV) else {
        return;
    };
    let snapshot = snapshot(Path::new(&project));
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("run interpreter tests");
    assert_eq!(results.len(), 4, "{results:#?}");
    assert!(
        results.iter().all(|result| result.failure.is_none()),
        "{results:#?}"
    );
}

#[test]
fn structured_values_match_in_interpreter_and_native_runtime() {
    assert_eq!(
        NATIVE_RUNTIME_ABI,
        loom_runtime_abi::NATIVE_RUNTIME_ABI_IDENTITY
    );
    assert!(NATIVE_RUNTIME_ABI.ends_with("/stdlib-v1"));
    assert!(native_runtime_identity().starts_with(NATIVE_RUNTIME_ABI));
    let project = tempfile::tempdir().expect("create standard-library project");
    let round_trip = project.path().join("round-trip.txt");
    let missing = project.path().join("missing.txt");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback fixture");
    let port = listener.local_addr().expect("loopback address").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept loopback fixture");
            let mut bytes = Vec::new();
            stream
                .read_to_end(&mut bytes)
                .expect("read loopback fixture");
            assert!(bytes.is_empty(), "empty write emitted bytes: {bytes:?}");
        }
    });
    let source = include_str!("../../../fixtures/standard-library/main.loom")
        .replace(
            "__ROUND_TRIP_PATH__",
            round_trip
                .to_str()
                .expect("temporary round-trip path is UTF-8"),
        )
        .replace(
            "__MISSING_PATH__",
            missing.to_str().expect("temporary missing path is UTF-8"),
        )
        .replace("__LOOPBACK_PORT__", &port.to_string());
    std::fs::write(project.path().join("main.loom"), source).expect("write fixture source");

    let interpreter = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "interpreter_standard_library_child",
            "--nocapture",
        ])
        .env(CHILD_PROJECT_ENV, project.path())
        .output()
        .expect("run interpreter fixture child");
    assert!(
        interpreter.status.success(),
        "status={:?} stdout={} stderr={}",
        interpreter.status,
        String::from_utf8_lossy(&interpreter.stdout),
        String::from_utf8_lossy(&interpreter.stderr),
    );
    assert_eq!(
        String::from_utf8(interpreter.stderr).expect("interpreter stderr is UTF-8"),
        EXPECTED_LOGS,
    );

    let snapshot = snapshot(project.path());
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let executable = project.path().join("native-tests");
    emit_native(
        snapshot.executable().expect("lower standard-library MIR"),
        &executable,
        &EmitOptions::tests(),
    )
    .expect("emit native standard-library tests");
    let native = Command::new(executable)
        .output()
        .expect("run native standard-library tests");
    assert!(
        native.status.success(),
        "status={:?} stdout={} stderr={}",
        native.status,
        String::from_utf8_lossy(&native.stdout),
        String::from_utf8_lossy(&native.stderr),
    );
    assert_eq!(
        String::from_utf8(native.stderr).expect("native stderr is UTF-8"),
        EXPECTED_LOGS,
    );
    let stdout = String::from_utf8(native.stdout).expect("native stdout is UTF-8");
    for test in [
        "text_map_and_json",
        "text_map_copy_and_gc",
        "typed_io",
        "canonical_logging",
    ] {
        assert!(
            stdout.contains(&format!("passed standard_library.{test}\n")),
            "{stdout}",
        );
    }
    assert_eq!(
        std::fs::read_to_string(round_trip).expect("read round-trip file"),
        "typed I/O",
    );
    server.join().expect("join loopback fixture");
}
