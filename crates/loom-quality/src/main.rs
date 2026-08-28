use std::fmt::Write as _;
use std::io::Read as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use loom_codegen_llvm::{
    EmitOptions, NativeArtifactKind, NativeRouteKind, NativeRoutePolicy, NativeTargetIdentity,
    OptimizationProfile, RuntimeBundle, RuntimeLinker, emit_native_debug_companion,
    emit_prepared_native_object, link_object_with_runtime_bundle, native_artifact_path,
    prepare_native_object, prepared_native_target_identity, target_identity,
};
use loom_core::{FileId, Span};
use loom_driver::AnalysisHost;
use loom_interpreter::{Interpreter, TestStatus, Value};
use loom_mir::{CheckedProgram, decode_interpreted_artifact, encode_interpreted_artifact};
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
const TYPED_LCIR_FIXTURE: &str = "fixtures/typed-lcir";
const TYPED_LOGGING_FIXTURE: &str = "fixtures/lcir-typed-logging";
const TYPED_LOGGING_STDERR: &[u8] =
    include_bytes!("../../../fixtures/lcir-typed-logging/expected.stderr");
const TYPED_ASYNC_FIXTURE: &str = "fixtures/lcir-typed-async";
const ASYNC_MANAGED_COLLECTIONS_FIXTURE: &str = "fixtures/lcir-async-managed-collections";
const TYPED_SLEEP_FIXTURE: &str = "fixtures/lcir-typed-sleep";
const SYNC_TASK_HELPERS_FIXTURE: &str = "fixtures/lcir-sync-task-helpers";
const TYPED_TASK_ALL_FIXTURE: &str = "fixtures/lcir-typed-task-all";
const TYPED_TASK_ANY_FIXTURE: &str = "fixtures/lcir-typed-task-any";
const TYPED_TASK_OUTCOMES_FIXTURE: &str = "fixtures/lcir-typed-task-outcomes";
const TYPED_ASYNC_CLEANUP_FIXTURE: &str = "fixtures/lcir-async-cleanup";
const TYPED_ASYNC_WRITEBACK_FIXTURE: &str = "fixtures/lcir-async-writeback";
const FALLIBLE_TYPED_ASYNC_FIXTURE: &str = "fixtures/lcir-fallible-async";
const QUALITY_EVIDENCE_SCHEMA_VERSION: u32 = 2;

const STANDARD_LIBRARY_LEGACY_ROUTE: NativeRouteExpectation =
    NativeRouteExpectation::LegacyAllowed {
        name: "standard-library-managed-runtime",
        reason: "JSON parse/format and typed external I/O are not yet complete in typed LCIR",
    };

const TASKS: &[TaskSpec] = &[
    TaskSpec {
        name: "constrained-contracts",
        path: "examples/core01",
        source: "examples/core01/shop.loom",
        sha256: "ddab0d78c8b21f94b902e628474047e4bae9df28895c79208ab2afe90113e009",
        main_native_route: NativeRouteExpectation::Lcir,
        test_native_route: NativeRouteExpectation::Lcir,
    },
    TaskSpec {
        name: "concept-polymorphism",
        path: "examples/core02",
        source: "examples/core02/concepts.loom",
        sha256: "7547be16a2b18c5d4b0693e457c1e2ef3ae877510b9bb085ce286e487da51d20",
        main_native_route: NativeRouteExpectation::Lcir,
        test_native_route: NativeRouteExpectation::Lcir,
    },
    TaskSpec {
        name: "structured-async",
        path: "examples/core03",
        source: "examples/core03/tasks.loom",
        sha256: "6d78ff8a8995952386c18e9befc03720a847e729b814f06a17ea097fb5fc06d1",
        main_native_route: NativeRouteExpectation::Lcir,
        test_native_route: NativeRouteExpectation::Lcir,
    },
];

struct TaskSpec {
    name: &'static str,
    path: &'static str,
    source: &'static str,
    sha256: &'static str,
    main_native_route: NativeRouteExpectation,
    test_native_route: NativeRouteExpectation,
}

#[derive(Clone, Copy)]
enum NativeRouteExpectation {
    Lcir,
    LegacyAllowed {
        name: &'static str,
        reason: &'static str,
    },
}

impl NativeRouteExpectation {
    const fn route(self) -> NativeRouteKind {
        match self {
            Self::Lcir => NativeRouteKind::Lcir,
            Self::LegacyAllowed { .. } => NativeRouteKind::Legacy,
        }
    }

    const fn legacy_allowlist(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Lcir => None,
            Self::LegacyAllowed { name, reason } => Some((name, reason)),
        }
    }
}

struct NativeRuntime {
    bundle: RuntimeBundle,
    linker: RuntimeLinker,
}

struct NativeBuild {
    functions: usize,
}

struct StandardLibraryFixture {
    round_trip: PathBuf,
    empty_write_listener: TcpListener,
    snapshot_listener: TcpListener,
}

struct FixtureServer {
    label: &'static str,
    audit_listener: TcpListener,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}

struct FixtureServers {
    cancel: Arc<AtomicBool>,
    servers: Vec<FixtureServer>,
}

impl FixtureServers {
    fn spawn(
        specifications: [(TcpListener, &'static [u8], &'static str); 2],
    ) -> Result<Self, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut group = Self {
            cancel,
            servers: Vec::with_capacity(specifications.len()),
        };
        for (listener, expected, label) in specifications {
            group.servers.push(spawn_fixture_server(
                listener,
                expected,
                label,
                Arc::clone(&group.cancel),
            )?);
        }
        Ok(group)
    }

    fn finish(mut self, backend: Result<(), String>) -> Result<(), String> {
        let reject_extra_connections = backend.is_ok();
        if !reject_extra_connections {
            self.cancel.store(true, Ordering::Release);
        }
        let mut failures = backend.err().into_iter().collect::<Vec<_>>();
        self.join_all(reject_extra_connections, &mut failures);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn join_all(&mut self, reject_extra_connections: bool, failures: &mut Vec<String>) {
        for server in &mut self.servers {
            if let Err(error) = server.join(reject_extra_connections) {
                failures.push(error);
            }
        }
    }
}

impl Drop for FixtureServers {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        let mut ignored = Vec::new();
        self.join_all(false, &mut ignored);
    }
}

impl FixtureServer {
    fn join(&mut self, reject_extra_connections: bool) -> Result<(), String> {
        let mut failures = Vec::new();
        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error),
                Err(_) => failures.push(format!("{} fixture thread panicked", self.label)),
            }
        }
        if reject_extra_connections {
            let mut extra_connections = 0_usize;
            loop {
                match self.audit_listener.accept() {
                    Ok((stream, _)) => {
                        extra_connections = extra_connections.saturating_add(1);
                        drop(stream);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => {
                        failures.push(format!("audit {} fixture connections: {error}", self.label));
                        break;
                    }
                }
            }
            if extra_connections != 0 {
                failures.push(format!(
                    "{} fixture received {extra_connections} unexpected additional connection(s)",
                    self.label
                ));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
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
    native_routes: Vec<NativeRouteEvidence>,
    gates: Vec<GateEvidence>,
    failures: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeRouteEvidence {
    scenario: String,
    expected: &'static str,
    actual: &'static str,
    legacy_allowlist: Option<&'static str>,
    legacy_reason: Option<&'static str>,
    passed: bool,
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

#[allow(clippy::too_many_lines)]
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
    let runtime = match load_native_runtime(&target) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("loom-quality: cannot load native runtime bundle: {error}");
            std::process::exit(2);
        }
    };
    let mut report = EvidenceReport {
        schema_version: QUALITY_EVIDENCE_SCHEMA_VERSION,
        evidence_level: "C3 real multi-package repository",
        status: "running",
        compiler_version: env!("CARGO_PKG_VERSION"),
        llvm_backend_version: loom_codegen_llvm::BACKEND_VERSION,
        interpreter_backend_version: loom_interpreter::BACKEND_VERSION,
        target_triple: target.triple,
        optimization: target.optimization,
        tasks: Vec::new(),
        repository: None,
        native_routes: Vec::new(),
        gates: Vec::new(),
        failures: Vec::new(),
    };

    for task in TASKS {
        match run_task(
            &workspace,
            task,
            &runtime,
            &mut report.gates,
            &mut report.native_routes,
        ) {
            Ok(evidence) => report.tasks.push(evidence),
            Err(error) => report.failures.push(format!("{}: {error}", task.name)),
        }
    }
    if let Err(error) = typed_lcir_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
    ) {
        report.failures.push(format!("typed-lcir: {error}"));
    }
    if let Err(error) = typed_logging_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
    ) {
        report.failures.push(format!("typed-logging: {error}"));
    }
    if let Err(error) = typed_async_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
        TYPED_ASYNC_FIXTURE,
        "typed-async",
    ) {
        report.failures.push(format!("typed-async: {error}"));
    }
    if let Err(error) = typed_async_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
        ASYNC_MANAGED_COLLECTIONS_FIXTURE,
        "async-managed-collections",
    ) {
        report
            .failures
            .push(format!("async-managed-collections: {error}"));
    }
    if let Err(error) = typed_async_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
        FALLIBLE_TYPED_ASYNC_FIXTURE,
        "fallible-typed-async",
    ) {
        report
            .failures
            .push(format!("fallible-typed-async: {error}"));
    }
    if let Err(error) = typed_async_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
        TYPED_SLEEP_FIXTURE,
        "typed-sleep",
    ) {
        report.failures.push(format!("typed-sleep: {error}"));
    }
    if let Err(error) = typed_async_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
        SYNC_TASK_HELPERS_FIXTURE,
        "sync-task-helpers",
    ) {
        report.failures.push(format!("sync-task-helpers: {error}"));
    }
    if let Err(error) = typed_async_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
        TYPED_TASK_ALL_FIXTURE,
        "typed-task-all",
    ) {
        report.failures.push(format!("typed-task-all: {error}"));
    }
    if let Err(error) = typed_async_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
        TYPED_TASK_ANY_FIXTURE,
        "typed-task-any",
    ) {
        report.failures.push(format!("typed-task-any: {error}"));
    }
    if let Err(error) = typed_async_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
        TYPED_TASK_OUTCOMES_FIXTURE,
        "typed-task-outcomes",
    ) {
        report
            .failures
            .push(format!("typed-task-outcomes: {error}"));
    }
    if let Err(error) = typed_async_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
        TYPED_ASYNC_CLEANUP_FIXTURE,
        "typed-async-cleanup",
    ) {
        report
            .failures
            .push(format!("typed-async-cleanup: {error}"));
    }
    if let Err(error) = typed_async_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
        TYPED_ASYNC_WRITEBACK_FIXTURE,
        "typed-async-writeback",
    ) {
        report
            .failures
            .push(format!("typed-async-writeback: {error}"));
    }
    match run_c3_repository(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
    ) {
        Ok(evidence) => report.repository = Some(evidence),
        Err(error) => report.failures.push(format!("c3-repository: {error}")),
    }
    if let Err(error) = async_generic_contract_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
    ) {
        report
            .failures
            .push(format!("async-generic-contracts: {error}"));
    }
    if let Err(error) = standard_library_gate(
        &workspace,
        &runtime,
        &mut report.gates,
        &mut report.native_routes,
    ) {
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

fn load_native_runtime(target: &NativeTargetIdentity) -> Result<NativeRuntime, String> {
    let root = if let Some(configured) = std::env::var_os("LOOM_RUNTIME_BUNDLE") {
        PathBuf::from(configured)
    } else {
        let executable = std::env::current_exe()
            .map_err(|error| format!("locate quality executable: {error}"))?;
        let executable = std::fs::canonicalize(&executable)
            .map_err(|error| format!("resolve {}: {error}", executable.display()))?;
        executable
            .parent()
            .ok_or_else(|| "resolved quality executable has no parent".to_owned())?
            .join("runtime")
    };
    let bundle = RuntimeBundle::load(root, target).map_err(|error| error.to_string())?;
    let linker = RuntimeLinker::load(std::env::var_os("LOOM_CC").unwrap_or_else(|| "clang".into()))
        .map_err(|error| error.to_string())?;
    Ok(NativeRuntime { bundle, linker })
}

fn native_route_name(route: NativeRouteKind) -> &'static str {
    match route {
        NativeRouteKind::Lcir => "lcir",
        NativeRouteKind::Legacy => "legacy",
    }
}

fn emit_routed_native(
    program: &CheckedProgram,
    output: &Path,
    options: EmitOptions,
    runtime: &NativeRuntime,
    scenario: impl Into<String>,
    expectation: NativeRouteExpectation,
    routes: &mut Vec<NativeRouteEvidence>,
) -> Result<NativeBuild, String> {
    let scenario = scenario.into();
    let has_debug_sources = !options.debug_sources.is_empty();
    let diagnostic_options = options.clone();
    let prepared = prepare_native_object(program, options, NativeRoutePolicy::Automatic)
        .map_err(|error| format!("{scenario} native preparation failed: {error}"))?;
    let actual = prepared.route_kind();
    let expected = expectation.route();
    let passed = actual == expected;
    let allowlist = expectation.legacy_allowlist();
    routes.push(NativeRouteEvidence {
        scenario: scenario.clone(),
        expected: native_route_name(expected),
        actual: native_route_name(actual),
        legacy_allowlist: allowlist.map(|(name, _)| name),
        legacy_reason: allowlist.map(|(_, reason)| reason),
        passed,
    });
    if !passed {
        let detail = if matches!(expectation, NativeRouteExpectation::Lcir) {
            match prepare_native_object(program, diagnostic_options, NativeRoutePolicy::LcirOnly) {
                Ok(_) => "LcirOnly unexpectedly prepared after automatic legacy routing".into(),
                Err(error) => error.to_string(),
            }
        } else {
            "route did not match its reviewed expectation".to_owned()
        };
        return Err(format!(
            "{scenario} selected native route `{}`, expected `{}`: {detail}",
            native_route_name(actual),
            native_route_name(expected),
        ));
    }

    let target = prepared_native_target_identity(&prepared);
    if runtime.bundle.target_triple() != target.triple
        || runtime.bundle.data_layout() != target.data_layout
    {
        return Err(format!(
            "{scenario} runtime bundle target does not match the prepared native object"
        ));
    }
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let object = native_artifact_path(
        directory.path().join("loom-quality"),
        Some(&target.triple),
        NativeArtifactKind::Object,
    );
    let emitted = emit_prepared_native_object(&prepared, &object)
        .map_err(|error| format!("{scenario} object emission failed: {error}"))?;
    link_object_with_runtime_bundle(&object, output, &runtime.bundle, &runtime.linker)
        .map_err(|error| format!("{scenario} native link failed: {error}"))?;
    if has_debug_sources {
        emit_native_debug_companion(output)
            .map_err(|error| format!("{scenario} debug companion failed: {error}"))?;
    }
    Ok(NativeBuild {
        functions: emitted.functions,
    })
}

fn typed_lcir_gate(
    workspace: &Path,
    runtime: &NativeRuntime,
    gates: &mut Vec<GateEvidence>,
    routes: &mut Vec<NativeRouteEvidence>,
) -> Result<(), String> {
    let project = workspace.join(TYPED_LCIR_FIXTURE);
    let snapshot = AnalysisHost::new(&project)
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    if snapshot.has_errors() {
        return Err(format!("source diagnostics: {:#?}", snapshot.diagnostics()));
    }
    let program = snapshot.executable().map_err(|error| error.to_string())?;
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let executable = directory.path().join("main");
    let build_started = Instant::now();
    emit_routed_native(
        program,
        &executable,
        EmitOptions::run("main").with_optimization(OptimizationProfile::Release),
        runtime,
        "typed-lcir.main",
        NativeRouteExpectation::Lcir,
        routes,
    )?;
    upper_gate(
        gates,
        "typed-lcir.native-build",
        build_started.elapsed(),
        NATIVE_BUILD_BUDGET,
    );

    let run_started = Instant::now();
    let output = Command::new(&executable)
        .current_dir(project)
        .output()
        .map_err(|error| format!("execute typed LCIR fixture: {error}"))?;
    if !output.status.success() || output.stdout != b"Unit\n" {
        return Err(format!(
            "native main mismatch: status={:?}, stdout={}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    upper_gate(
        gates,
        "typed-lcir.native-execution",
        run_started.elapsed(),
        EXECUTION_BUDGET,
    );
    Ok(())
}

fn typed_logging_gate(
    workspace: &Path,
    runtime: &NativeRuntime,
    gates: &mut Vec<GateEvidence>,
    routes: &mut Vec<NativeRouteEvidence>,
) -> Result<(), String> {
    let project = workspace.join(TYPED_LOGGING_FIXTURE);
    let analysis_started = Instant::now();
    let snapshot = AnalysisHost::new(&project)
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    upper_gate(
        gates,
        "typed-logging.analysis",
        analysis_started.elapsed(),
        ANALYSIS_BUDGET,
    );
    if snapshot.has_errors() {
        return Err(format!("source diagnostics: {:#?}", snapshot.diagnostics()));
    }
    let program = snapshot.executable().map_err(|error| error.to_string())?;
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let executable = directory.path().join("main");
    let tests = directory.path().join("tests");

    let build_started = Instant::now();
    emit_routed_native(
        program,
        &executable,
        EmitOptions::run("main").with_optimization(OptimizationProfile::Release),
        runtime,
        "typed-logging.main",
        NativeRouteExpectation::Lcir,
        routes,
    )?;
    emit_routed_native(
        program,
        &tests,
        EmitOptions::tests().with_optimization(OptimizationProfile::Release),
        runtime,
        "typed-logging.tests",
        NativeRouteExpectation::Lcir,
        routes,
    )?;
    upper_gate(
        gates,
        "typed-logging.native-build",
        build_started.elapsed(),
        NATIVE_BUILD_BUDGET,
    );

    let run_started = Instant::now();
    let output = Command::new(&executable)
        .current_dir(&project)
        .output()
        .map_err(|error| format!("execute typed logging fixture: {error}"))?;
    if !output.status.success()
        || output.stdout != b"Unit\n"
        || output.stderr.as_slice() != TYPED_LOGGING_STDERR
    {
        return Err(format!(
            "native main mismatch: status={:?}, stdout={}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    let output = Command::new(&tests)
        .current_dir(&project)
        .output()
        .map_err(|error| format!("execute typed logging tests: {error}"))?;
    if !output.status.success()
        || output.stdout != b"passed lcir_typed_logging.typedLogging\n"
        || output.stderr.as_slice() != TYPED_LOGGING_STDERR
    {
        return Err(format!(
            "native test mismatch: status={:?}, stdout={}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    upper_gate(
        gates,
        "typed-logging.native-execution",
        run_started.elapsed(),
        EXECUTION_BUDGET,
    );
    Ok(())
}

fn typed_async_gate(
    workspace: &Path,
    runtime: &NativeRuntime,
    gates: &mut Vec<GateEvidence>,
    routes: &mut Vec<NativeRouteEvidence>,
    fixture: &str,
    scenario: &str,
) -> Result<(), String> {
    let project = workspace.join(fixture);
    let snapshot = AnalysisHost::new(&project)
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    if snapshot.has_errors() {
        return Err(format!("source diagnostics: {:#?}", snapshot.diagnostics()));
    }
    let program = snapshot.executable().map_err(|error| error.to_string())?;
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let executable = directory.path().join("main");
    let tests = directory.path().join("tests");
    let build_started = Instant::now();
    emit_routed_native(
        program,
        &executable,
        EmitOptions::run("main").with_optimization(OptimizationProfile::Release),
        runtime,
        format!("{scenario}.main"),
        NativeRouteExpectation::Lcir,
        routes,
    )?;
    emit_routed_native(
        program,
        &tests,
        EmitOptions::tests().with_optimization(OptimizationProfile::Release),
        runtime,
        format!("{scenario}.tests"),
        NativeRouteExpectation::Lcir,
        routes,
    )?;
    upper_gate(
        gates,
        &format!("{scenario}.native-build"),
        build_started.elapsed(),
        NATIVE_BUILD_BUDGET,
    );

    let run_started = Instant::now();
    let output = Command::new(&executable)
        .current_dir(&project)
        .output()
        .map_err(|error| format!("execute {scenario} fixture: {error}"))?;
    if !output.status.success() || output.stdout != b"Unit\n" {
        return Err(format!(
            "native main mismatch: status={:?}, stdout={}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let output = Command::new(&tests)
        .current_dir(&project)
        .output()
        .map_err(|error| format!("execute {scenario} tests: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success()
        || stdout.lines().count() != program.tests.len()
        || !stdout.lines().all(|line| line.starts_with("passed "))
    {
        return Err(format!(
            "native test mismatch: status={:?}, stdout={}, stderr={}",
            output.status.code(),
            stdout,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    upper_gate(
        gates,
        &format!("{scenario}.native-execution"),
        run_started.elapsed(),
        EXECUTION_BUDGET,
    );
    Ok(())
}

fn standard_library_gate(
    workspace: &Path,
    runtime: &NativeRuntime,
    gates: &mut Vec<GateEvidence>,
    routes: &mut Vec<NativeRouteEvidence>,
) -> Result<(), String> {
    let interpreter_project = tempfile::tempdir().map_err(|error| error.to_string())?;
    let interpreter_fixture =
        prepare_standard_library_fixture(workspace, interpreter_project.path())?;
    let native_project = tempfile::tempdir().map_err(|error| error.to_string())?;
    let native_fixture = prepare_standard_library_fixture(workspace, native_project.path())?;

    let analysis_started = Instant::now();
    let interpreter_snapshot = AnalysisHost::new(interpreter_project.path())
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    let native_snapshot = AnalysisHost::new(native_project.path())
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    upper_gate(
        gates,
        "standard-library.analysis",
        analysis_started.elapsed(),
        ANALYSIS_BUDGET,
    );
    if interpreter_snapshot.has_errors() {
        return Err(format!(
            "interpreter source diagnostics: {:#?}",
            interpreter_snapshot.diagnostics()
        ));
    }
    if native_snapshot.has_errors() {
        return Err(format!(
            "native source diagnostics: {:#?}",
            native_snapshot.diagnostics()
        ));
    }
    let interpreter_program = interpreter_snapshot
        .executable()
        .map_err(|error| error.to_string())?;
    let native_program = native_snapshot
        .executable()
        .map_err(|error| error.to_string())?;

    run_standard_library_interpreter(interpreter_program, interpreter_fixture, gates)?;
    run_standard_library_native(
        native_program,
        native_project.path(),
        native_fixture,
        runtime,
        gates,
        routes,
    )
}

fn run_standard_library_interpreter(
    program: &CheckedProgram,
    fixture: StandardLibraryFixture,
    gates: &mut Vec<GateEvidence>,
) -> Result<(), String> {
    let servers = FixtureServers::spawn([
        (fixture.empty_write_listener, b"", "interpreter empty write"),
        (
            fixture.snapshot_listener,
            b"socket snapshot",
            "interpreter socket snapshot",
        ),
    ])?;

    let interpreter_started = Instant::now();
    let interpreted = Interpreter::new(program).run_tests();
    let backend = if interpreted.len() != program.tests.len()
        || interpreted
            .iter()
            .any(|result| result.status != TestStatus::Passed)
    {
        Err(format!(
            "interpreter tests did not all pass: {interpreted:#?}"
        ))
    } else {
        Ok(())
    };
    servers.finish(backend)?;
    if std::fs::read_to_string(&fixture.round_trip).map_err(|error| error.to_string())?
        != "typed I/O"
    {
        return Err("interpreter typed file round trip did not preserve text".to_owned());
    }
    upper_gate(
        gates,
        "standard-library.interpreter-execution",
        interpreter_started.elapsed(),
        EXECUTION_BUDGET,
    );
    Ok(())
}

fn run_standard_library_native(
    program: &CheckedProgram,
    project: &Path,
    fixture: StandardLibraryFixture,
    runtime: &NativeRuntime,
    gates: &mut Vec<GateEvidence>,
    routes: &mut Vec<NativeRouteEvidence>,
) -> Result<(), String> {
    let executable = project.join("native-tests");
    let native_build_started = Instant::now();
    emit_routed_native(
        program,
        &executable,
        EmitOptions::tests().with_optimization(OptimizationProfile::Release),
        runtime,
        "standard-library.tests",
        STANDARD_LIBRARY_LEGACY_ROUTE,
        routes,
    )
    .map_err(|error| format!("native test build failed: {error}"))?;
    upper_gate(
        gates,
        "standard-library.native-build",
        native_build_started.elapsed(),
        NATIVE_BUILD_BUDGET,
    );

    let servers = FixtureServers::spawn([
        (fixture.empty_write_listener, b"", "native empty write"),
        (
            fixture.snapshot_listener,
            b"socket snapshot",
            "native socket snapshot",
        ),
    ])?;
    let native_run_started = Instant::now();
    let backend = Command::new(&executable)
        .current_dir(project)
        .env("LOOM_FAULT_FORMAT", "json")
        .output()
        .map_err(|error| format!("execute native tests: {error}"))
        .and_then(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if output.status.success()
                && stdout.lines().count() == program.tests.len()
                && stdout.lines().all(|line| line.starts_with("passed "))
            {
                Ok(())
            } else {
                Err(format!(
                    "native test mismatch: status={:?}, stdout={}, stderr={}",
                    output.status.code(),
                    stdout,
                    String::from_utf8_lossy(&output.stderr)
                ))
            }
        });
    servers.finish(backend)?;
    if std::fs::read_to_string(&fixture.round_trip).map_err(|error| error.to_string())?
        != "typed I/O"
    {
        return Err("native typed file round trip did not preserve text".to_owned());
    }
    upper_gate(
        gates,
        "standard-library.native-execution",
        native_run_started.elapsed(),
        EXECUTION_BUDGET,
    );
    Ok(())
}

fn prepare_standard_library_fixture(
    workspace: &Path,
    project: &Path,
) -> Result<StandardLibraryFixture, String> {
    let round_trip = project.join("round-trip.txt");
    let reuse = project.join("reuse.txt");
    let missing = project.join("missing.txt");
    let empty_write_listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("bind empty-write fixture: {error}"))?;
    let empty_write_port = empty_write_listener
        .local_addr()
        .map_err(|error| format!("read empty-write fixture address: {error}"))?
        .port();
    let snapshot_listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("bind socket-snapshot fixture: {error}"))?;
    let snapshot_port = snapshot_listener
        .local_addr()
        .map_err(|error| format!("read socket-snapshot fixture address: {error}"))?
        .port();
    let source = std::fs::read_to_string(workspace.join(STANDARD_LIBRARY_FIXTURE))
        .map_err(|error| error.to_string())?
        .replace("__ROUND_TRIP_PATH__", &loom_text_literal(&round_trip))
        .replace("__REUSE_PATH__", &loom_text_literal(&reuse))
        .replace("__MISSING_PATH__", &loom_text_literal(&missing))
        .replace("__LOOPBACK_PORT__", &empty_write_port.to_string())
        .replace("__READ_LOOPBACK_PORT__", &snapshot_port.to_string());
    std::fs::write(project.join("main.loom"), source).map_err(|error| error.to_string())?;
    Ok(StandardLibraryFixture {
        round_trip,
        empty_write_listener,
        snapshot_listener,
    })
}

fn spawn_fixture_server(
    listener: TcpListener,
    expected: &'static [u8],
    label: &'static str,
    cancel: Arc<AtomicBool>,
) -> Result<FixtureServer, String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("configure {label} listener: {error}"))?;
    let audit_listener = listener
        .try_clone()
        .map_err(|error| format!("clone {label} listener: {error}"))?;
    let thread = std::thread::Builder::new()
        .name(format!("loom-quality-{label}"))
        .spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(120);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if cancel.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        if Instant::now() >= deadline {
                            return Err(format!("{label} fixture timed out before connection"));
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => return Err(format!("accept {label} fixture: {error}")),
                }
            };
            stream
                .set_nonblocking(false)
                .map_err(|error| format!("configure {label} stream blocking mode: {error}"))?;
            stream
                .set_read_timeout(Some(EXECUTION_BUDGET))
                .map_err(|error| format!("configure {label} stream: {error}"))?;
            let mut bytes = Vec::new();
            stream
                .read_to_end(&mut bytes)
                .map_err(|error| format!("read {label} fixture: {error}"))?;
            if bytes != expected {
                return Err(format!(
                    "{label} fixture expected {expected:?}, received {bytes:?}"
                ));
            }
            Ok(())
        })
        .map_err(|error| format!("spawn {label} fixture thread: {error}"))?;
    Ok(FixtureServer {
        label,
        audit_listener,
        thread: Some(thread),
    })
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
    runtime: &NativeRuntime,
    gates: &mut Vec<GateEvidence>,
    routes: &mut Vec<NativeRouteEvidence>,
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
    emit_routed_native(
        program,
        &executable,
        EmitOptions::tests().with_optimization(OptimizationProfile::Release),
        runtime,
        "async-generic-contracts.tests",
        NativeRouteExpectation::Lcir,
        routes,
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
    runtime: &NativeRuntime,
    gates: &mut Vec<GateEvidence>,
    routes: &mut Vec<NativeRouteEvidence>,
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
    let main_artifact = emit_routed_native(
        program,
        &executable,
        EmitOptions::run(&entry).with_optimization(OptimizationProfile::Release),
        runtime,
        "c3-repository.main",
        NativeRouteExpectation::Lcir,
        routes,
    )
    .map_err(|error| format!("native main build failed: {error}"))?;
    let test_executable = directory.path().join("tests");
    let test_artifact = emit_routed_native(
        program,
        &test_executable,
        EmitOptions::tests().with_optimization(OptimizationProfile::Release),
        runtime,
        "c3-repository.tests",
        NativeRouteExpectation::Lcir,
        routes,
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
    runtime: &NativeRuntime,
    gates: &mut Vec<GateEvidence>,
    routes: &mut Vec<NativeRouteEvidence>,
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
    let main_artifact = emit_routed_native(
        program,
        &executable,
        EmitOptions::run("main").with_optimization(OptimizationProfile::Release),
        runtime,
        format!("{}.main", task.name),
        task.main_native_route,
        routes,
    )
    .map_err(|error| format!("native main build failed: {error}"))?;
    let test_executable = directory.path().join("tests");
    let test_artifact = emit_routed_native(
        program,
        &test_executable,
        EmitOptions::tests().with_optimization(OptimizationProfile::Release),
        runtime,
        format!("{}.tests", task.name),
        task.test_native_route,
        routes,
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

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::net::{SocketAddr, TcpStream};

    use super::*;

    fn loopback_listener() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
        let address = listener.local_addr().expect("read test listener address");
        (listener, address)
    }

    #[test]
    fn fixture_server_rejects_an_additional_connection() {
        let (listener, address) = loopback_listener();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut server =
            spawn_fixture_server(listener, b"", "exactly one", cancel).expect("spawn server");

        drop(TcpStream::connect(address).expect("connect expected client"));
        drop(TcpStream::connect(address).expect("connect additional client"));

        let error = server
            .join(true)
            .expect_err("an additional connection must fail the fixture");
        assert!(error.contains("1 unexpected additional connection(s)"));
    }

    #[test]
    fn fixture_server_group_joins_and_reports_every_server_failure() {
        let (first_listener, first_address) = loopback_listener();
        let (second_listener, second_address) = loopback_listener();
        let servers = FixtureServers::spawn([
            (first_listener, b"first", "first server"),
            (second_listener, b"second", "second server"),
        ])
        .expect("spawn server group");

        let mut first = TcpStream::connect(first_address).expect("connect first client");
        first.write_all(b"wrong first").expect("write first client");
        drop(first);
        let mut second = TcpStream::connect(second_address).expect("connect second client");
        second
            .write_all(b"wrong second")
            .expect("write second client");
        drop(second);

        let error = servers
            .finish(Ok(()))
            .expect_err("both fixture failures must propagate");
        assert!(error.contains("first server fixture expected"));
        assert!(error.contains("second server fixture expected"));
    }

    #[test]
    fn fixture_server_group_cancels_and_joins_after_backend_failure() {
        let (first_listener, _) = loopback_listener();
        let (second_listener, _) = loopback_listener();
        let servers = FixtureServers::spawn([
            (first_listener, b"", "first waiting server"),
            (second_listener, b"", "second waiting server"),
        ])
        .expect("spawn server group");
        let started = Instant::now();

        let error = servers
            .finish(Err("backend failed".to_owned()))
            .expect_err("backend failure must propagate");

        assert_eq!(error, "backend failed");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
