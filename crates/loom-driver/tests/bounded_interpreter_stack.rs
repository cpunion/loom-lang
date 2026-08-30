use std::path::PathBuf;

use loom_core::Span;
use loom_driver::{AnalysisHost, ProjectOptions};
use loom_interpreter::{Interpreter, TestStatus, Value};

const ONE_MIB: usize = 1024 * 1024;

#[test]
fn source_json_fixture_runs_on_a_one_mib_interpreter_stack() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/lcir-json-parse")
        .canonicalize()
        .expect("canonical source JSON fixture");

    std::thread::Builder::new()
        .name("loom-one-mib-source-json".into())
        .stack_size(ONE_MIB)
        .spawn(move || {
            let snapshot = AnalysisHost::new_with_options(
                &fixture,
                &ProjectOptions {
                    tests: loom_driver::TestSelection::Recursive,
                    ..ProjectOptions::default()
                },
            )
            .expect("source JSON analysis host")
            .snapshot()
            .expect("source JSON snapshot");
            assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
            let program = snapshot.executable().expect("source JSON executable");
            let main = program
                .functions
                .iter()
                .find(|function| function.name == "standalone.main")
                .expect("source JSON main")
                .id;

            let run = Interpreter::new(program).invoke(main, Vec::new(), Span::default());
            assert!(matches!(run, Ok(Value::Unit)), "source JSON run: {run:?}");

            let tests = Interpreter::new(program).run_tests();
            assert_eq!(tests.len(), 1, "{tests:#?}");
            assert_eq!(tests[0].status, TestStatus::Passed, "{tests:#?}");
        })
        .expect("spawn source JSON interpreter on a Windows-sized stack")
        .join()
        .expect("source JSON interpreter must not overflow the host stack");
}
