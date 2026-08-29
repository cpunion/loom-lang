use std::path::Path;
use std::process::Command;

use loom_codegen_llvm::EmitOptions;

mod support;
use support::emit_native;

#[test]
fn generic_async_contracts_witnesses_and_cancellation_execute_natively() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let source = workspace.join("fixtures/async-generic-contracts");
    let snapshot = support::analysis_host(source)
        .expect("load generic async fixture")
        .snapshot()
        .expect("analyze generic async fixture");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower generic async fixture");

    let directory = tempfile::tempdir().expect("create native output directory");
    let executable = directory.path().join("tests");
    emit_native(program, &executable, &EmitOptions::tests())
        .expect("emit generic async native tests");
    let output = Command::new(executable)
        .output()
        .expect("run generic async native tests");
    assert!(
        output.status.success(),
        "status={:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 output"),
        "passed standalone.generic_async_contracts_and_cancellation\n"
    );
}
