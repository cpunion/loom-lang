use std::path::Path;

use loom_driver::AnalysisHost;
use loom_interpreter::TestStatus;

#[test]
fn generic_async_contracts_witnesses_and_cancellation_execute_in_interpreter() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let source = workspace.join("fixtures/async-generic-contracts");
    let snapshot = AnalysisHost::new(source)
        .expect("load generic async fixture")
        .snapshot()
        .expect("analyze generic async fixture");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());

    let results = snapshot.run_tests().expect("run generic async fixture");
    assert_eq!(results.len(), 1, "{results:#?}");
    assert_eq!(results[0].status, TestStatus::Passed, "{results:#?}");
}
