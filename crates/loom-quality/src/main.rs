use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use loom_codegen_llvm::{EmitOptions, OptimizationProfile, emit_native, target_identity};
use loom_core::{FileId, Span};
use loom_driver::AnalysisHost;
use loom_interpreter::{Interpreter, TestStatus, Value};
use loom_mir::{decode_interpreted_artifact, encode_interpreted_artifact};
use loom_syntax::{lex, parse_with_file};
use serde::Serialize;
use sha2::{Digest, Sha256};

const ANALYSIS_BUDGET: Duration = Duration::from_secs(10);
const NATIVE_BUILD_BUDGET: Duration = Duration::from_secs(60);
const EXECUTION_BUDGET: Duration = Duration::from_secs(15);
const PARSER_BUDGET: Duration = Duration::from_secs(8);
const ARTIFACT_DECODE_BUDGET: Duration = Duration::from_secs(15);
const INCREMENTAL_BUDGET: Duration = Duration::from_secs(10);
const TOTAL_BUDGET: Duration = Duration::from_secs(300);
const C3_ANALYSIS_BUDGET: Duration = Duration::from_secs(15);
const C3_NATIVE_BUILD_BUDGET: Duration = Duration::from_secs(90);
const C3_REPOSITORY: &str = "examples/c3/application";
const C3_TARGET: &str = "app";
const ASYNC_GENERIC_FIXTURE: &str = "fixtures/async-generic-contracts";
const STANDARD_LIBRARY_FIXTURE: &str = "fixtures/standard-library/main.loom";

const TASKS: &[TaskSpec] = &[
    TaskSpec {
        name: "constrained-contracts",
        path: "examples/core01",
        source: "examples/core01/shop.loom",
        sha256: "f3c6b8cad23cf4113e7555ac29d2307d853af10eff4ee89482ef4c8617a77472",
    },
    TaskSpec {
        name: "concept-polymorphism",
        path: "examples/core02",
        source: "examples/core02/concepts.loom",
        sha256: "60bc7e21bd475ae3fb0f795f25cbae92e4d86c7c48675abad02c9561d2701d4a",
    },
    TaskSpec {
        name: "structured-async",
        path: "examples/core03",
        source: "examples/core03/tasks.loom",
        sha256: "0981e9597a0a450c4a4bc035568be1e57fe50bb746dd827c7471aab45c0dae2d",
    },
];

struct TaskSpec {
    name: &'static str,
    path: &'static str,
    source: &'static str,
    sha256: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceReport {
    schema_version: u32,
    evidence_level: &'static str,
    status: &'static str,
    compiler_version: &'static str,
    llvm_backend_version: &'static str,
    interpreter_backend_version: &'static str,
    target_triple: String,
    optimization: String,
    tasks: Vec<TaskEvidence>,
    repository: Option<RepositoryEvidence>,
    gates: Vec<GateEvidence>,
    failures: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryEvidence {
    name: &'static str,
    path: &'static str,
    source_sha256: String,
    packages: usize,
    modules: usize,
    source_bytes: usize,
    mir_functions: usize,
    mir_tests: usize,
    native_reachable_main_functions: usize,
    native_reachable_test_functions: usize,
    analysis_ms: u64,
    interpreter_run_ms: u64,
    native_build_ms: u64,
    native_run_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskEvidence {
    name: &'static str,
    source: &'static str,
    source_sha256: String,
    source_bytes: usize,
    source_lines: usize,
    tokens: usize,
    mir_functions: usize,
    mir_tests: usize,
    interpreter_tests: usize,
    native_reachable_main_functions: usize,
    native_reachable_test_functions: usize,
    analysis_ms: u64,
    interpreter_run_ms: u64,
    native_build_ms: u64,
    native_run_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GateEvidence {
    name: String,
    measured: u64,
    unit: &'static str,
    expectation: String,
    passed: bool,
}

fn main() {
    let started = Instant::now();
    let workspace = workspace_root();
    let target = match target_identity(None, OptimizationProfile::Release) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("loom-quality: cannot initialize LLVM target: {error}");
            std::process::exit(2);
        }
    };
    let mut report = EvidenceReport {
        schema_version: 1,
        evidence_level: "C3 real multi-package repository",
        status: "running",
        compiler_version: env!("CARGO_PKG_VERSION"),
        llvm_backend_version: loom_codegen_llvm::BACKEND_VERSION,
        interpreter_backend_version: loom_interpreter::BACKEND_VERSION,
        target_triple: target.triple,
        optimization: target.optimization,
        tasks: Vec::new(),
        repository: None,
        gates: Vec::new(),
        failures: Vec::new(),
    };

    for task in TASKS {
        match run_task(&workspace, task, &mut report.gates) {
            Ok(evidence) => report.tasks.push(evidence),
            Err(error) => report.failures.push(format!("{}: {error}", task.name)),
        }
    }
    match run_c3_repository(&workspace, &mut report.gates) {
        Ok(evidence) => report.repository = Some(evidence),
        Err(error) => report.failures.push(format!("c3-repository: {error}")),
    }
    if let Err(error) = async_generic_contract_gate(&workspace, &mut report.gates) {
        report
            .failures
            .push(format!("async-generic-contracts: {error}"));
    }
    if let Err(error) = standard_library_gate(&workspace, &mut report.gates) {
        report.failures.push(format!("standard-library: {error}"));
    }
    if let Err(error) = parser_throughput_gate(&workspace, &mut report.gates) {
        report.failures.push(error);
    }
    if let Err(error) = artifact_decode_gate(&workspace, &mut report.gates) {
        report.failures.push(error);
    }
    if let Err(error) = incremental_query_gate(&mut report.gates) {
        report.failures.push(error);
    }
    upper_gate(
        &mut report.gates,
        "controlled-suite-total",
        started.elapsed(),
        TOTAL_BUDGET,
    );
    let passed = report.failures.is_empty() && report.gates.iter().all(|gate| gate.passed);
    report.status = if passed { "passed" } else { "failed" };

    serde_json::to_writer_pretty(std::io::stdout().lock(), &report)
        .expect("serialize quality evidence");
    println!();
    if !passed {
        std::process::exit(1);
    }
}

fn standard_library_gate(workspace: &Path, gates: &mut Vec<GateEvidence>) -> Result<(), String> {
    let project = tempfile::tempdir().map_err(|error| error.to_string())?;
    let round_trip = project.path().join("round-trip.txt");
    let missing = project.path().join("missing.txt");
    let source = std::fs::read_to_string(workspace.join(STANDARD_LIBRARY_FIXTURE))
        .map_err(|error| error.to_string())?
        .replace("__ROUND_TRIP_PATH__", &loom_text_literal(&round_trip))
        .replace("__MISSING_PATH__", &loom_text_literal(&missing));
    std::fs::write(project.path().join("main.loom"), source).map_err(|error| error.to_string())?;

    let analysis_started = Instant::now();
    let snapshot = AnalysisHost::new(project.path())
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    upper_gate(
        gates,
        "standard-library.analysis",
        analysis_started.elapsed(),
        ANALYSIS_BUDGET,
    );
    if snapshot.has_errors() {
        return Err(format!("source diagnostics: {:#?}", snapshot.diagnostics()));
    }
    let program = snapshot.executable().map_err(|error| error.to_string())?;

    let interpreter_started = Instant::now();
    let interpreted = Interpreter::new(program).run_tests();
    if interpreted.len() != program.tests.len()
        || interpreted
            .iter()
            .any(|result| result.status != TestStatus::Passed)
    {
        return Err(format!(
            "interpreter tests did not all pass: {interpreted:#?}"
        ));
    }
    upper_gate(
        gates,
        "standard-library.interpreter-execution",
        interpreter_started.elapsed(),
        EXECUTION_BUDGET,
    );

    let executable = project.path().join("native-tests");
    let native_build_started = Instant::now();
    emit_native(
        program,
        &executable,
        &EmitOptions::tests().with_optimization(OptimizationProfile::Release),
    )
    .map_err(|error| format!("native test build failed: {error}"))?;
    upper_gate(
        gates,
        "standard-library.native-build",
        native_build_started.elapsed(),
        NATIVE_BUILD_BUDGET,
    );

    let native_run_started = Instant::now();
    let output = Command::new(&executable)
        .current_dir(project.path())
        .output()
        .map_err(|error| format!("execute native tests: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success()
        || stdout.lines().count() != program.tests.len()
        || !stdout.lines().all(|line| line.starts_with("passed "))
    {
        return Err(format!(
            "native test mismatch: status={:?}, stdout={}, stderr={}",
            output.status.code(),
            stdout,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if std::fs::read_to_string(&round_trip).map_err(|error| error.to_string())? != "typed I/O" {
        return Err("typed file round trip did not preserve text".to_owned());
    }
    upper_gate(
        gates,
        "standard-library.native-execution",
        native_run_started.elapsed(),
        EXECUTION_BUDGET,
    );
    Ok(())
}

fn loom_text_literal(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn async_generic_contract_gate(
    workspace: &Path,
    gates: &mut Vec<GateEvidence>,
) -> Result<(), String> {
    let snapshot = AnalysisHost::new(workspace.join(ASYNC_GENERIC_FIXTURE))
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    if snapshot.has_errors() {
        return Err(format!("source diagnostics: {:#?}", snapshot.diagnostics()));
    }
    let program = snapshot.executable().map_err(|error| error.to_string())?;

    let interpreter_started = Instant::now();
    let interpreter_results = Interpreter::new(program).run_tests();
    if interpreter_results.len() != 1
        || interpreter_results
            .iter()
            .any(|result| result.status != TestStatus::Passed)
    {
        return Err(format!(
            "interpreter tests did not pass exactly once: {interpreter_results:#?}"
        ));
    }
    upper_gate(
        gates,
        "async-generic-contracts.interpreter-execution",
        interpreter_started.elapsed(),
        EXECUTION_BUDGET,
    );

    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let executable = directory.path().join("tests");
    let native_build_started = Instant::now();
    emit_native(
        program,
        &executable,
        &EmitOptions::tests().with_optimization(OptimizationProfile::Release),
    )
    .map_err(|error| format!("native test build failed: {error}"))?;
    upper_gate(
        gates,
        "async-generic-contracts.native-build",
        native_build_started.elapsed(),
        NATIVE_BUILD_BUDGET,
    );

    let native_run_started = Instant::now();
    let output = Command::new(&executable)
        .current_dir(workspace.join(ASYNC_GENERIC_FIXTURE))
        .output()
        .map_err(|error| format!("execute native tests: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success()
        || stdout != "passed async_generic_contracts.generic_async_contracts_and_cancellation\n"
    {
        return Err(format!(
            "native test mismatch: status={:?}, stdout={}, stderr={}",
            output.status.code(),
            stdout,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    upper_gate(
        gates,
        "async-generic-contracts.native-execution",
        native_run_started.elapsed(),
        EXECUTION_BUDGET,
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn run_c3_repository(
    workspace: &Path,
    gates: &mut Vec<GateEvidence>,
) -> Result<RepositoryEvidence, String> {
    let analysis_started = Instant::now();
    let snapshot = AnalysisHost::new(workspace.join(C3_REPOSITORY))
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    let analysis_elapsed = analysis_started.elapsed();
    upper_gate(
        gates,
        "c3-repository.analysis",
        analysis_elapsed,
        C3_ANALYSIS_BUDGET,
    );
    if snapshot.has_errors() {
        return Err(format!("source diagnostics: {:#?}", snapshot.diagnostics()));
    }

    let entry = snapshot
        .project()
        .target(C3_TARGET)
        .and_then(loom_driver::Target::entry)
        .ok_or_else(|| "C3 app target has no entry".to_owned())?
        .to_owned();
    let program = snapshot.executable().map_err(|error| error.to_string())?;
    let main = program
        .exports
        .get(&entry)
        .copied()
        .ok_or_else(|| format!("C3 program does not export `{entry}`"))?;

    let interpreter_started = Instant::now();
    let interpreter_tests = Interpreter::new(program).run_tests();
    if interpreter_tests
        .iter()
        .any(|result| result.status != TestStatus::Passed)
    {
        return Err(format!(
            "interpreter tests did not pass: {interpreter_tests:#?}"
        ));
    }
    let call_site = program
        .function(main)
        .map_or_else(Span::default, |function| function.span);
    let interpreted = Interpreter::new(program)
        .invoke(main, Vec::new(), call_site)
        .map_err(|failure| format!("interpreter main failed: {failure:?}"))?;
    if interpreted != Value::Unit {
        return Err(format!(
            "interpreter main returned {}, expected Unit",
            interpreted.summary()
        ));
    }
    let interpreter_elapsed = interpreter_started.elapsed();
    upper_gate(
        gates,
        "c3-repository.interpreter-execution",
        interpreter_elapsed,
        EXECUTION_BUDGET,
    );

    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let native_build_started = Instant::now();
    let executable = directory.path().join("main");
    let main_artifact = emit_native(
        program,
        &executable,
        &EmitOptions::run(&entry).with_optimization(OptimizationProfile::Release),
    )
    .map_err(|error| format!("native main build failed: {error}"))?;
    let test_executable = directory.path().join("tests");
    let test_artifact = emit_native(
        program,
        &test_executable,
        &EmitOptions::tests().with_optimization(OptimizationProfile::Release),
    )
    .map_err(|error| format!("native test build failed: {error}"))?;
    let native_build_elapsed = native_build_started.elapsed();
    upper_gate(
        gates,
        "c3-repository.native-build",
        native_build_elapsed,
        C3_NATIVE_BUILD_BUDGET,
    );

    let native_run_started = Instant::now();
    let main_output = Command::new(&executable)
        .current_dir(workspace.join(C3_REPOSITORY))
        .output()
        .map_err(|error| format!("execute native main: {error}"))?;
    if !main_output.status.success() || main_output.stdout != b"Unit\n" {
        return Err(format!(
            "native main mismatch: status={:?}, stdout={}, stderr={}",
            main_output.status.code(),
            String::from_utf8_lossy(&main_output.stdout),
            String::from_utf8_lossy(&main_output.stderr)
        ));
    }
    let test_output = Command::new(&test_executable)
        .current_dir(workspace.join(C3_REPOSITORY))
        .output()
        .map_err(|error| format!("execute native tests: {error}"))?;
    let native_test_output = String::from_utf8_lossy(&test_output.stdout);
    if !test_output.status.success()
        || native_test_output.lines().count() != program.tests.len()
        || !native_test_output
            .lines()
            .all(|line| line.starts_with("passed "))
    {
        return Err(format!(
            "native tests mismatch: status={:?}, stdout={}, stderr={}",
            test_output.status.code(),
            native_test_output,
            String::from_utf8_lossy(&test_output.stderr)
        ));
    }
    let native_run_elapsed = native_run_started.elapsed();
    upper_gate(
        gates,
        "c3-repository.native-execution",
        native_run_elapsed,
        EXECUTION_BUDGET,
    );

    let mut source_identity = Sha256::new();
    let mut source_bytes = 0_usize;
    for document in snapshot.sources().documents() {
        let text = document
            .text()
            .ok_or_else(|| format!("{} is not UTF-8", document.relative_path()))?;
        source_identity.update(document.relative_path().as_bytes());
        source_identity.update([0]);
        source_identity.update(text.as_bytes());
        source_identity.update([0]);
        source_bytes = source_bytes.saturating_add(text.len());
    }
    let source_sha256 =
        source_identity
            .finalize()
            .iter()
            .fold(String::with_capacity(64), |mut output, byte| {
                write!(output, "{byte:02x}").expect("writing to a String cannot fail");
                output
            });

    Ok(RepositoryEvidence {
        name: "multi-package-checkout-service",
        path: C3_REPOSITORY,
        source_sha256,
        packages: snapshot.project().packages().count(),
        modules: snapshot.sources().documents().len(),
        source_bytes,
        mir_functions: program.functions.len(),
        mir_tests: program.tests.len(),
        native_reachable_main_functions: main_artifact.functions,
        native_reachable_test_functions: test_artifact.functions,
        analysis_ms: millis(analysis_elapsed),
        interpreter_run_ms: millis(interpreter_elapsed),
        native_build_ms: millis(native_build_elapsed),
        native_run_ms: millis(native_run_elapsed),
    })
}

#[allow(clippy::too_many_lines)]
fn run_task(
    workspace: &Path,
    task: &'static TaskSpec,
    gates: &mut Vec<GateEvidence>,
) -> Result<TaskEvidence, String> {
    let source = std::fs::read_to_string(workspace.join(task.source))
        .map_err(|error| format!("read {}: {error}", task.source))?;
    let source_sha256 = sha256(source.as_bytes());
    if source_sha256 != task.sha256 {
        return Err(format!(
            "fixture hash changed: expected {}, found {source_sha256}",
            task.sha256
        ));
    }

    let analysis_started = Instant::now();
    let snapshot = AnalysisHost::new(workspace.join(task.path))
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    let analysis_elapsed = analysis_started.elapsed();
    upper_gate(
        gates,
        &format!("{}.analysis", task.name),
        analysis_elapsed,
        ANALYSIS_BUDGET,
    );
    if snapshot.has_errors() {
        return Err(format!("source diagnostics: {:#?}", snapshot.diagnostics()));
    }
    let program = snapshot.executable().map_err(|error| error.to_string())?;

    let interpreter_started = Instant::now();
    let interpreter_tests = Interpreter::new(program).run_tests();
    if interpreter_tests
        .iter()
        .any(|result| result.status != TestStatus::Passed)
    {
        return Err(format!(
            "interpreter tests did not pass: {interpreter_tests:#?}"
        ));
    }
    let main = program
        .exports
        .get("main")
        .copied()
        .ok_or_else(|| "fixture has no `main` export".to_owned())?;
    let call_site = program
        .function(main)
        .map_or_else(Span::default, |function| function.span);
    let interpreted = Interpreter::new(program)
        .invoke(main, Vec::new(), call_site)
        .map_err(|failure| format!("interpreter main failed: {failure:?}"))?;
    if interpreted != Value::Unit {
        return Err(format!(
            "interpreter main returned {}, expected Unit",
            interpreted.summary()
        ));
    }
    let interpreter_elapsed = interpreter_started.elapsed();
    upper_gate(
        gates,
        &format!("{}.interpreter-execution", task.name),
        interpreter_elapsed,
        EXECUTION_BUDGET,
    );

    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let native_build_started = Instant::now();
    let executable = directory.path().join("main");
    let main_artifact = emit_native(
        program,
        &executable,
        &EmitOptions::run("main").with_optimization(OptimizationProfile::Release),
    )
    .map_err(|error| format!("native main build failed: {error}"))?;
    let test_executable = directory.path().join("tests");
    let test_artifact = emit_native(
        program,
        &test_executable,
        &EmitOptions::tests().with_optimization(OptimizationProfile::Release),
    )
    .map_err(|error| format!("native test build failed: {error}"))?;
    let native_build_elapsed = native_build_started.elapsed();
    upper_gate(
        gates,
        &format!("{}.native-build", task.name),
        native_build_elapsed,
        NATIVE_BUILD_BUDGET,
    );

    let native_run_started = Instant::now();
    let main_output = Command::new(&executable)
        .current_dir(workspace.join(task.path))
        .output()
        .map_err(|error| format!("execute native main: {error}"))?;
    if !main_output.status.success() || main_output.stdout != b"Unit\n" {
        return Err(format!(
            "native main mismatch: status={:?}, stdout={}, stderr={}",
            main_output.status.code(),
            String::from_utf8_lossy(&main_output.stdout),
            String::from_utf8_lossy(&main_output.stderr)
        ));
    }
    let test_output = Command::new(&test_executable)
        .current_dir(workspace.join(task.path))
        .output()
        .map_err(|error| format!("execute native tests: {error}"))?;
    let native_test_output = String::from_utf8_lossy(&test_output.stdout);
    if !test_output.status.success()
        || native_test_output.lines().count() != program.tests.len()
        || !native_test_output
            .lines()
            .all(|line| line.starts_with("passed "))
    {
        return Err(format!(
            "native tests mismatch: status={:?}, stdout={}, stderr={}",
            test_output.status.code(),
            native_test_output,
            String::from_utf8_lossy(&test_output.stderr)
        ));
    }
    let native_run_elapsed = native_run_started.elapsed();
    upper_gate(
        gates,
        &format!("{}.native-execution", task.name),
        native_run_elapsed,
        EXECUTION_BUDGET,
    );

    Ok(TaskEvidence {
        name: task.name,
        source: task.source,
        source_sha256,
        source_bytes: source.len(),
        source_lines: source.lines().count(),
        tokens: lex(&source).tokens.len(),
        mir_functions: program.functions.len(),
        mir_tests: program.tests.len(),
        interpreter_tests: interpreter_tests.len(),
        native_reachable_main_functions: main_artifact.functions,
        native_reachable_test_functions: test_artifact.functions,
        analysis_ms: millis(analysis_elapsed),
        interpreter_run_ms: millis(interpreter_elapsed),
        native_build_ms: millis(native_build_elapsed),
        native_run_ms: millis(native_run_elapsed),
    })
}

fn parser_throughput_gate(workspace: &Path, gates: &mut Vec<GateEvidence>) -> Result<(), String> {
    let source = std::fs::read_to_string(workspace.join(TASKS[2].source))
        .map_err(|error| error.to_string())?
        .repeat(256);
    let started = Instant::now();
    let parse = parse_with_file(FileId(17), &source);
    let elapsed = started.elapsed();
    if parse.reconstructed() != source {
        return Err("parser scale fixture was not lossless".to_owned());
    }
    upper_gate(gates, "parser-1.8mb-lossless", elapsed, PARSER_BUDGET);
    Ok(())
}

fn artifact_decode_gate(workspace: &Path, gates: &mut Vec<GateEvidence>) -> Result<(), String> {
    let snapshot = AnalysisHost::new(workspace.join(TASKS[2].path))
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    if snapshot.has_errors() {
        return Err("artifact throughput fixture has diagnostics".to_owned());
    }
    let bytes =
        encode_interpreted_artifact(snapshot.executable().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let started = Instant::now();
    for _ in 0..32 {
        decode_interpreted_artifact(&bytes).map_err(|error| error.to_string())?;
    }
    upper_gate(
        gates,
        "checked-mir-artifact-decode-32x",
        started.elapsed(),
        ARTIFACT_DECODE_BUDGET,
    );
    Ok(())
}

fn incremental_query_gate(gates: &mut Vec<GateEvidence>) -> Result<(), String> {
    const MODULES: usize = 64;
    let project = tempfile::tempdir().map_err(|error| error.to_string())?;
    for index in 0..MODULES {
        let source =
            format!("module scale.m{index}\n\npub fn value{index}() Int {{\n    {index}\n}}\n");
        std::fs::write(project.path().join(format!("m{index}.loom")), source)
            .map_err(|error| error.to_string())?;
    }
    let host = AnalysisHost::new(project.path()).map_err(|error| error.to_string())?;
    let initial = host.snapshot().map_err(|error| error.to_string())?;
    if initial.has_errors() {
        return Err(format!(
            "incremental fixture diagnostics: {:#?}",
            initial.diagnostics()
        ));
    }
    let changed_path = project.path().join("m63.loom");
    std::fs::write(
        changed_path,
        "module scale.m63\n\npub fn value63() Int {\n    64\n}\n",
    )
    .map_err(|error| error.to_string())?;
    let started = Instant::now();
    let changed = host.snapshot().map_err(|error| error.to_string())?;
    let elapsed = started.elapsed();
    if changed.has_errors() {
        return Err(format!(
            "changed incremental fixture diagnostics: {:#?}",
            changed.diagnostics()
        ));
    }
    let stats = changed.semantic_query_stats();
    lower_gate(
        gates,
        "typed-hir-modules-reused-after-one-body-edit",
        u64::try_from(stats.modules_reused).unwrap_or(u64::MAX),
        u64::try_from(MODULES - 1).expect("module count fits u64"),
        "modules",
    );
    upper_value_gate(
        gates,
        "typed-hir-modules-rechecked-after-one-body-edit",
        u64::try_from(stats.modules_checked).unwrap_or(u64::MAX),
        1,
        "modules",
    );
    upper_gate(
        gates,
        "typed-hir-one-body-edit-latency",
        elapsed,
        INCREMENTAL_BUDGET,
    );
    Ok(())
}

fn upper_gate(gates: &mut Vec<GateEvidence>, name: &str, measured: Duration, maximum: Duration) {
    upper_value_gate(gates, name, millis(measured), millis(maximum), "ms");
}

fn upper_value_gate(
    gates: &mut Vec<GateEvidence>,
    name: &str,
    measured: u64,
    maximum: u64,
    unit: &'static str,
) {
    gates.push(GateEvidence {
        name: name.to_owned(),
        measured,
        unit,
        expectation: format!("<= {maximum}"),
        passed: measured <= maximum,
    });
}

fn lower_gate(
    gates: &mut Vec<GateEvidence>,
    name: &str,
    measured: u64,
    minimum: u64,
    unit: &'static str,
) {
    gates.push(GateEvidence {
        name: name.to_owned(),
        measured,
        unit,
        expectation: format!(">= {minimum}"),
        passed: measured >= minimum,
    });
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        })
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("quality crate is inside the workspace")
        .to_path_buf()
}
