use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use loom_core::{FileId, Span};
use loom_driver::{
    AnalysisHost, AnalysisSnapshot, CacheContext, CacheKey, CacheLookup, DiagnosticRecord,
    EXIT_DEFECT, EXIT_FAILURE, EXIT_SUCCESS, EXIT_USAGE, LockMode, PersistentCache, PipelineStage,
    ProjectGraph, ProjectOptions, SourceMap, StageUnavailable, TargetKind, format_source,
};
use loom_interpreter::TestStatus;
use serde_json::{Value, json};

const USAGE: &str = "usage: loomc [--json] [--backend llvm|interpreter] [--features A,B] [--no-default-features] [--locked] [--no-cache | --cache-dir DIR] <resolve|check|build|test|run|fmt> [options] [PATH]\n\
    resolve [--update] [PATH] resolve dependencies and materialize loom.lock\n\
    check [--target NAME] [PATH] parse, lower, and type-check a project\n\
    build [--target NAME | --entry NAME] [--output FILE] [PATH] build an executable or portable library\n\
    test [--target NAME] [PATH] compile and execute ordinary test fn declarations\n\
    run [--target NAME | --entry NAME] [PATH] [-- ARGS...] compile and execute an exported function\n\
    run --artifact FILE [-- ARGS...] execute a previously built artifact\n\
    fmt [--check] [PATH]     format .loom files (default PATH is .)";

const DEFAULT_NATIVE_ARTIFACT: &str = "target/loom/program";
const DEFAULT_INTERPRETED_ARTIFACT: &str = "target/loom/program.loomi";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Backend {
    Llvm,
    Interpreter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Resolve {
        refresh: bool,
    },
    Check,
    Build {
        output: PathBuf,
        entry: Option<String>,
    },
    Test,
    Run {
        entry: Option<String>,
        artifact: Option<PathBuf>,
    },
    Format {
        check: bool,
    },
}

#[derive(Clone, Debug)]
struct Options {
    command: Command,
    path: PathBuf,
    json: bool,
    backend: Backend,
    target: Option<String>,
    no_cache: bool,
    cache_dir: Option<PathBuf>,
    project: ProjectOptions,
    program_arguments: Vec<String>,
}

enum ParsedArgs {
    Run(Options),
    Help,
    Version,
}

struct TargetSelectionError {
    code: &'static str,
    message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheStatus {
    Disabled,
    Hit,
    Miss,
}

impl CacheStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Hit => "hit",
            Self::Miss => "miss",
        }
    }
}

enum CompilationData {
    Fresh(Box<AnalysisSnapshot>),
    Cached(Box<CachedCompilationData>),
}

struct CachedCompilationData {
    project: ProjectGraph,
    sources: SourceMap,
    program: loom_mir::Program,
    diagnostics: Vec<DiagnosticRecord>,
}

struct Compilation {
    data: CompilationData,
    cache: Option<PersistentCache>,
    key: Option<CacheKey>,
}

impl Compilation {
    fn project(&self) -> &ProjectGraph {
        match &self.data {
            CompilationData::Fresh(snapshot) => snapshot.project(),
            CompilationData::Cached(cached) => &cached.project,
        }
    }

    fn sources(&self) -> &SourceMap {
        match &self.data {
            CompilationData::Fresh(snapshot) => snapshot.sources(),
            CompilationData::Cached(cached) => &cached.sources,
        }
    }

    fn diagnostic_records(&self) -> Vec<DiagnosticRecord> {
        match &self.data {
            CompilationData::Fresh(snapshot) => snapshot.diagnostic_records(),
            CompilationData::Cached(cached) => cached.diagnostics.clone(),
        }
    }

    fn has_errors(&self) -> bool {
        match &self.data {
            CompilationData::Fresh(snapshot) => snapshot.has_errors(),
            CompilationData::Cached(cached) => cached
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == "error"),
        }
    }

    fn require_stage(&self, requested: PipelineStage) -> Result<(), StageUnavailable> {
        match &self.data {
            CompilationData::Fresh(snapshot) => snapshot.require_stage(requested),
            CompilationData::Cached(_) => Ok(()),
        }
    }

    fn executable(&self) -> Result<&loom_mir::Program, StageUnavailable> {
        match &self.data {
            CompilationData::Fresh(snapshot) => snapshot.executable(),
            CompilationData::Cached(cached) => Ok(&cached.program),
        }
    }

    fn run_tests(&self) -> Result<Vec<loom_interpreter::TestResult>, StageUnavailable> {
        match &self.data {
            CompilationData::Fresh(snapshot) => snapshot.run_tests(),
            CompilationData::Cached(cached) => {
                Ok(loom_interpreter::Interpreter::new(&cached.program).run_tests())
            }
        }
    }

    const fn cache(&self) -> Option<&PersistentCache> {
        self.cache.as_ref()
    }

    const fn key(&self) -> Option<&CacheKey> {
        self.key.as_ref()
    }
}

impl TargetSelectionError {
    fn unknown(name: &str) -> Self {
        Self {
            code: "UnknownTarget",
            message: format!("manifest does not define target `{name}`"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BuildTarget {
    Binary(String),
    Library(String),
}

fn target_kind_mismatch(name: &str, actual: TargetKind, required: &str) -> TargetSelectionError {
    TargetSelectionError {
        code: "TargetKindMismatch",
        message: format!(
            "target `{name}` is `{}`, but this command requires {required}",
            actual.as_str()
        ),
    }
}

fn select_binary_target(
    project: &ProjectGraph,
    requested: Option<&str>,
    explicit_entry: Option<&str>,
) -> Result<String, TargetSelectionError> {
    if let Some(name) = requested {
        let target = project
            .target(name)
            .ok_or_else(|| TargetSelectionError::unknown(name))?;
        if target.kind() != TargetKind::Bin {
            return Err(target_kind_mismatch(name, target.kind(), "`bin`"));
        }
        return Ok(target
            .entry()
            .expect("validated bin target has an entry")
            .to_owned());
    }
    if let Some(entry) = explicit_entry {
        return Ok(entry.to_owned());
    }
    let binaries = project
        .targets()
        .iter()
        .filter(|target| target.kind() == TargetKind::Bin)
        .collect::<Vec<_>>();
    match binaries.as_slice() {
        [] => Ok("main".to_owned()),
        [target] => Ok(target
            .entry()
            .expect("validated bin target has an entry")
            .to_owned()),
        _ => Err(TargetSelectionError {
            code: "AmbiguousTarget",
            message: format!(
                "multiple binary targets are available: {}; pass --target NAME",
                binaries
                    .iter()
                    .map(|target| target.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

fn select_build_target(
    project: &ProjectGraph,
    requested: Option<&str>,
    explicit_entry: Option<&str>,
) -> Result<BuildTarget, TargetSelectionError> {
    if let Some(name) = requested {
        let target = project
            .target(name)
            .ok_or_else(|| TargetSelectionError::unknown(name))?;
        return match target.kind() {
            TargetKind::Bin => Ok(BuildTarget::Binary(
                target
                    .entry()
                    .expect("validated bin target has an entry")
                    .to_owned(),
            )),
            TargetKind::Lib => Ok(BuildTarget::Library(target.name().to_owned())),
            TargetKind::Test => Err(target_kind_mismatch(name, target.kind(), "`bin` or `lib`")),
        };
    }
    if let Some(entry) = explicit_entry {
        return Ok(BuildTarget::Binary(entry.to_owned()));
    }

    let binaries = project
        .targets()
        .iter()
        .filter(|target| target.kind() == TargetKind::Bin)
        .collect::<Vec<_>>();
    match binaries.as_slice() {
        [target] => {
            return Ok(BuildTarget::Binary(
                target
                    .entry()
                    .expect("validated bin target has an entry")
                    .to_owned(),
            ));
        }
        [] => {}
        _ => {
            return Err(TargetSelectionError {
                code: "AmbiguousTarget",
                message: format!(
                    "multiple binary targets are available: {}; pass --target NAME",
                    binaries
                        .iter()
                        .map(|target| target.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
    }

    let libraries = project
        .targets()
        .iter()
        .filter(|target| target.kind() == TargetKind::Lib)
        .collect::<Vec<_>>();
    match libraries.as_slice() {
        [target] => Ok(BuildTarget::Library(target.name().to_owned())),
        [] if project.targets().is_empty() => Ok(BuildTarget::Binary("main".to_owned())),
        [] => Err(TargetSelectionError {
            code: "NoBuildTarget",
            message: "manifest does not define a `bin` or `lib` target".to_owned(),
        }),
        _ => Err(TargetSelectionError {
            code: "AmbiguousTarget",
            message: format!(
                "multiple library targets are available: {}; pass --target NAME",
                libraries
                    .iter()
                    .map(|target| target.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

fn select_test_target(
    project: &ProjectGraph,
    requested: Option<&str>,
) -> Result<(), TargetSelectionError> {
    if let Some(name) = requested {
        let target = project
            .target(name)
            .ok_or_else(|| TargetSelectionError::unknown(name))?;
        if target.kind() != TargetKind::Test {
            return Err(target_kind_mismatch(name, target.kind(), "`test`"));
        }
        return Ok(());
    }
    let tests = project
        .targets()
        .iter()
        .filter(|target| target.kind() == TargetKind::Test)
        .collect::<Vec<_>>();
    if tests.len() <= 1 {
        Ok(())
    } else {
        Err(TargetSelectionError {
            code: "AmbiguousTarget",
            message: format!(
                "multiple test targets are available: {}; pass --target NAME",
                tests
                    .iter()
                    .map(|target| target.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })
    }
}

/// Binary entry point, kept testable without spawning a subprocess.
#[must_use]
pub fn main_entry() -> i32 {
    let arguments = std::env::args_os().skip(1);
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    match run(arguments, &mut stdout, &mut stderr) {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(stderr, "loomc: {error}");
            EXIT_DEFECT
        }
    }
}

/// Runs the CLI against caller-provided streams.
///
/// # Errors
///
/// Returns an I/O error if a command cannot write its report or complete an
/// explicitly requested filesystem operation.
pub fn run(
    arguments: impl IntoIterator<Item = OsString>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let parsed = match parse_arguments(arguments) {
        Ok(parsed) => parsed,
        Err(message) => {
            writeln!(stderr, "loomc: {message}\n{USAGE}")?;
            return Ok(EXIT_USAGE);
        }
    };
    let options = match parsed {
        ParsedArgs::Help => {
            writeln!(stdout, "{USAGE}")?;
            return Ok(EXIT_SUCCESS);
        }
        ParsedArgs::Version => {
            writeln!(stdout, "loomc {}", env!("CARGO_PKG_VERSION"))?;
            return Ok(EXIT_SUCCESS);
        }
        ParsedArgs::Run(options) => options,
    };

    match &options.command {
        Command::Resolve { refresh } => run_resolve(&options, *refresh, stdout, stderr),
        Command::Format { check } => run_format(&options, *check, stdout, stderr),
        Command::Check => run_check(&options, stdout, stderr),
        Command::Build { output, entry } => {
            run_build(&options, output, entry.as_deref(), stdout, stderr)
        }
        Command::Test => run_test(&options, stdout, stderr),
        Command::Run { entry, artifact } => {
            if let Some(artifact) = artifact {
                run_artifact(&options, artifact, entry.as_deref(), stdout, stderr)
            } else {
                run_program(&options, entry.as_deref(), stdout, stderr)
            }
        }
    }
}

fn project_options(options: &Options, refresh: bool) -> ProjectOptions {
    let mut project = options.project.clone();
    if refresh {
        project.lock_mode = LockMode::Refresh;
    }
    project
}

fn open_analysis_host(
    options: &Options,
    refresh: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<Option<AnalysisHost>> {
    let host =
        match AnalysisHost::new_with_options(&options.path, &project_options(options, refresh)) {
            Ok(host) => host,
            Err(error) => {
                emit_tool_error(
                    options.json,
                    stdout,
                    stderr,
                    "ProjectLoadFailed",
                    &error.to_string(),
                )?;
                return Ok(None);
            }
        };
    if options.project.lock_mode != LockMode::Locked
        && let Err(error) = host.project().write_lockfile()
    {
        emit_tool_error(
            options.json,
            stdout,
            stderr,
            "LockfileWriteFailed",
            &error.to_string(),
        )?;
        return Ok(None);
    }
    Ok(Some(host))
}

fn run_resolve(
    options: &Options,
    refresh: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let Some(host) = open_analysis_host(options, refresh, stdout, stderr)? else {
        return Ok(EXIT_USAGE);
    };
    let Some(lockfile) = host.project().lockfile_path() else {
        emit_tool_error(
            options.json,
            stdout,
            stderr,
            "ManifestRequired",
            "resolve requires a loom.toml project",
        )?;
        return Ok(EXIT_USAGE);
    };
    if options.json {
        write_json_line(
            stdout,
            &json!({
                "schema_version": 1,
                "category": "dependency_resolution",
                "status": "ok",
                "packages": host.project().packages().count(),
                "lockfile": lockfile,
                "refreshed": refresh,
            }),
        )?;
    } else {
        writeln!(
            stdout,
            "resolved {} packages into {}",
            host.project().packages().count(),
            lockfile.display()
        )?;
    }
    Ok(EXIT_SUCCESS)
}

#[allow(clippy::too_many_lines)]
fn run_build(
    options: &Options,
    output: &Path,
    explicit_entry: Option<&str>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let Some(compilation) = load_compilation(options, stdout, stderr)? else {
        return Ok(EXIT_USAGE);
    };
    if emit_source_diagnostics(&compilation, options.json, stdout, stderr)? {
        return Ok(EXIT_FAILURE);
    }
    let target = match select_build_target(
        compilation.project(),
        options.target.as_deref(),
        explicit_entry,
    ) {
        Ok(target) => target,
        Err(error) => return emit_target_error(options, stdout, stderr, &error),
    };
    let program = match compilation.executable() {
        Ok(program) => program,
        Err(unavailable) => {
            emit_unavailable(&unavailable, options.json, stdout, stderr)?;
            return Ok(EXIT_USAGE);
        }
    };
    // An explicit artifact path follows ordinary CLI rules and is resolved
    // from the caller's working directory, independently of the source root.
    let output = output.to_path_buf();
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        emit_tool_error(
            options.json,
            stdout,
            stderr,
            "ArtifactWriteFailed",
            &format!("{}: {error}", output.display()),
        )?;
        return Ok(EXIT_USAGE);
    }
    if let BuildTarget::Library(name) = &target {
        return build_library(
            &compilation,
            program,
            name,
            &output,
            options,
            stdout,
            stderr,
        );
    }
    let BuildTarget::Binary(entry) = target else {
        unreachable!("library target returned above")
    };
    if !program.exports.contains_key(&entry) {
        emit_tool_error(
            options.json,
            stdout,
            stderr,
            "UnknownEntry",
            &format!("no exported entry named `{entry}`"),
        )?;
        return Ok(EXIT_FAILURE);
    }
    let artifact_key =
        final_artifact_key(&compilation, program, options.backend, "run", Some(&entry));
    match restore_cached_artifact(
        &compilation,
        artifact_key.as_ref(),
        &output,
        options.backend == Backend::Llvm,
        options,
        stdout,
    )? {
        Ok(true) => {
            emit_build_result(options, stdout, &output)?;
            return Ok(EXIT_SUCCESS);
        }
        Ok(false) => {}
        Err(message) => {
            emit_tool_error(
                options.json,
                stdout,
                stderr,
                "ArtifactWriteFailed",
                &message,
            )?;
            return Ok(EXIT_USAGE);
        }
    }
    match options.backend {
        Backend::Llvm => {
            if let Err(error) = emit_native_with_cache(
                &compilation,
                program,
                &output,
                &loom_codegen_llvm::EmitOptions::run(&entry),
                "run",
                Some(&entry),
                options,
                stdout,
            )? {
                emit_tool_error(options.json, stdout, stderr, error.code(), error.message())?;
                return Ok(EXIT_DEFECT);
            }
        }
        Backend::Interpreter => {
            let bytes = match loom_mir::encode_interpreted_executable_artifact(program, &entry) {
                Ok(bytes) => bytes,
                Err(error) => {
                    emit_tool_error(
                        options.json,
                        stdout,
                        stderr,
                        "CompilerDefect",
                        &error.to_string(),
                    )?;
                    return Ok(EXIT_DEFECT);
                }
            };
            if let Err(error) = std::fs::write(&output, bytes) {
                emit_tool_error(
                    options.json,
                    stdout,
                    stderr,
                    "ArtifactWriteFailed",
                    &format!("{}: {error}", output.display()),
                )?;
                return Ok(EXIT_USAGE);
            }
        }
    }
    store_artifact_best_effort(&compilation, artifact_key.as_ref(), &output);
    emit_build_result(options, stdout, &output)?;
    Ok(EXIT_SUCCESS)
}

fn build_library(
    compilation: &Compilation,
    program: &loom_mir::Program,
    target: &str,
    output: &Path,
    options: &Options,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let artifact_key = library_artifact_key(compilation, target);
    match restore_cached_artifact(
        compilation,
        artifact_key.as_ref(),
        output,
        false,
        options,
        stdout,
    )? {
        Ok(true) => {
            emit_build_result(options, stdout, output)?;
            return Ok(EXIT_SUCCESS);
        }
        Ok(false) => {}
        Err(message) => {
            emit_tool_error(
                options.json,
                stdout,
                stderr,
                "ArtifactWriteFailed",
                &message,
            )?;
            return Ok(EXIT_USAGE);
        }
    }
    let bytes = match loom_mir::encode_interpreted_artifact(program) {
        Ok(bytes) => bytes,
        Err(error) => {
            emit_tool_error(
                options.json,
                stdout,
                stderr,
                "CompilerDefect",
                &error.to_string(),
            )?;
            return Ok(EXIT_DEFECT);
        }
    };
    if let Err(error) = std::fs::write(output, bytes) {
        emit_tool_error(
            options.json,
            stdout,
            stderr,
            "ArtifactWriteFailed",
            &format!("{}: {error}", output.display()),
        )?;
        return Ok(EXIT_USAGE);
    }
    store_artifact_best_effort(compilation, artifact_key.as_ref(), output);
    emit_build_result(options, stdout, output)?;
    Ok(EXIT_SUCCESS)
}

fn run_check(options: &Options, stdout: &mut dyn Write, stderr: &mut dyn Write) -> io::Result<i32> {
    let Some(compilation) = load_compilation(options, stdout, stderr)? else {
        return Ok(EXIT_USAGE);
    };
    if emit_source_diagnostics(&compilation, options.json, stdout, stderr)? {
        return Ok(EXIT_FAILURE);
    }
    if let Some(target) = options.target.as_deref()
        && compilation.project().target(target).is_none()
    {
        return emit_target_error(
            options,
            stdout,
            stderr,
            &TargetSelectionError::unknown(target),
        );
    }
    if let Err(unavailable) = compilation.require_stage(PipelineStage::TypeChecked) {
        emit_unavailable(&unavailable, options.json, stdout, stderr)?;
        return Ok(EXIT_USAGE);
    }
    emit_summary(
        options.json,
        stdout,
        "check",
        compilation.sources().documents().len(),
    )?;
    Ok(EXIT_SUCCESS)
}

fn run_test(options: &Options, stdout: &mut dyn Write, stderr: &mut dyn Write) -> io::Result<i32> {
    let Some(compilation) = load_compilation(options, stdout, stderr)? else {
        return Ok(EXIT_USAGE);
    };
    if emit_source_diagnostics(&compilation, options.json, stdout, stderr)? {
        return Ok(EXIT_FAILURE);
    }
    if let Err(error) = select_test_target(compilation.project(), options.target.as_deref()) {
        return emit_target_error(options, stdout, stderr, &error);
    }
    if options.backend == Backend::Llvm {
        let program = match compilation.executable() {
            Ok(program) => program,
            Err(unavailable) => {
                emit_unavailable(&unavailable, options.json, stdout, stderr)?;
                return Ok(EXIT_USAGE);
            }
        };
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("loom-tests");
        let artifact_key =
            final_artifact_key(&compilation, program, options.backend, "tests", None);
        match restore_cached_artifact(
            &compilation,
            artifact_key.as_ref(),
            &executable,
            true,
            options,
            stdout,
        )? {
            Ok(true) => {
                return execute_native_with_options(options, &executable, stdout, stderr);
            }
            Ok(false) => {}
            Err(message) => {
                emit_tool_error(
                    options.json,
                    stdout,
                    stderr,
                    "ArtifactWriteFailed",
                    &message,
                )?;
                return Ok(EXIT_USAGE);
            }
        }
        if let Err(error) = emit_native_with_cache(
            &compilation,
            program,
            &executable,
            &loom_codegen_llvm::EmitOptions::tests(),
            "tests",
            None,
            options,
            stdout,
        )? {
            emit_tool_error(options.json, stdout, stderr, error.code(), error.message())?;
            return Ok(EXIT_DEFECT);
        }
        store_artifact_best_effort(&compilation, artifact_key.as_ref(), &executable);
        return execute_native_with_options(options, &executable, stdout, stderr);
    }
    let results = match compilation.run_tests() {
        Ok(results) => results,
        Err(unavailable) => {
            emit_unavailable(&unavailable, options.json, stdout, stderr)?;
            return Ok(EXIT_USAGE);
        }
    };
    let mut failed = false;
    for result in &results {
        failed |= result.status == TestStatus::Failed;
        if options.json {
            write_json_line(
                stdout,
                &json!({
                    "schema_version": 1,
                    "category": "test_result",
                    "result": result,
                }),
            )?;
        } else {
            let status = if result.status == TestStatus::Passed {
                "passed"
            } else {
                "failed"
            };
            writeln!(stdout, "{status} {}", result.name)?;
        }
    }
    Ok(if failed { EXIT_FAILURE } else { EXIT_SUCCESS })
}

fn run_program(
    options: &Options,
    explicit_entry: Option<&str>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let Some(compilation) = load_compilation(options, stdout, stderr)? else {
        return Ok(EXIT_USAGE);
    };
    if emit_source_diagnostics(&compilation, options.json, stdout, stderr)? {
        return Ok(EXIT_FAILURE);
    }
    let entry = match select_binary_target(
        compilation.project(),
        options.target.as_deref(),
        explicit_entry,
    ) {
        Ok(entry) => entry,
        Err(error) => return emit_target_error(options, stdout, stderr, &error),
    };
    let program = match compilation.executable() {
        Ok(program) => program,
        Err(unavailable) => {
            emit_unavailable(&unavailable, options.json, stdout, stderr)?;
            return Ok(EXIT_USAGE);
        }
    };
    match options.backend {
        Backend::Llvm => {
            let directory = tempfile::tempdir()?;
            let executable = directory.path().join("loom-program");
            let artifact_key =
                final_artifact_key(&compilation, program, options.backend, "run", Some(&entry));
            match restore_cached_artifact(
                &compilation,
                artifact_key.as_ref(),
                &executable,
                true,
                options,
                stdout,
            )? {
                Ok(true) => {
                    return execute_native_with_options(options, &executable, stdout, stderr);
                }
                Ok(false) => {}
                Err(message) => {
                    emit_tool_error(
                        options.json,
                        stdout,
                        stderr,
                        "ArtifactWriteFailed",
                        &message,
                    )?;
                    return Ok(EXIT_USAGE);
                }
            }
            if let Err(error) = emit_native_with_cache(
                &compilation,
                program,
                &executable,
                &loom_codegen_llvm::EmitOptions::run(&entry),
                "run",
                Some(&entry),
                options,
                stdout,
            )? {
                emit_tool_error(options.json, stdout, stderr, error.code(), error.message())?;
                return Ok(if error.code() == "UnknownEntry" {
                    EXIT_FAILURE
                } else {
                    EXIT_DEFECT
                });
            }
            store_artifact_best_effort(&compilation, artifact_key.as_ref(), &executable);
            execute_native_with_options(options, &executable, stdout, stderr)
        }
        Backend::Interpreter => invoke_program(
            program,
            &entry,
            &options.program_arguments,
            options.json,
            stdout,
            stderr,
        ),
    }
}

fn run_artifact(
    options: &Options,
    artifact: &Path,
    explicit_entry: Option<&str>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    if explicit_entry.is_some() {
        emit_tool_error(
            options.json,
            stdout,
            stderr,
            "ArtifactEntryFixedAtBuild",
            "an artifact's entry is selected by `build --entry` or `build --target`, not `run --entry`",
        )?;
        return Ok(EXIT_USAGE);
    }
    if options.backend == Backend::Llvm {
        return execute_native_with_options(options, artifact, stdout, stderr);
    }
    let json_output = options.json;
    let bytes = match std::fs::read(artifact) {
        Ok(bytes) => bytes,
        Err(error) => {
            emit_tool_error(
                json_output,
                stdout,
                stderr,
                "ArtifactLoadFailed",
                &format!("{}: {error}", artifact.display()),
            )?;
            return Ok(EXIT_USAGE);
        }
    };
    let (program, entry) = match loom_mir::decode_interpreted_executable_artifact(&bytes) {
        Ok(artifact) => artifact,
        Err(error) => {
            let code = if matches!(error, loom_mir::ArtifactError::VersionMismatch { .. }) {
                "ArtifactVersionMismatch"
            } else {
                "ArtifactLoadFailed"
            };
            emit_tool_error(json_output, stdout, stderr, code, &error.to_string())?;
            return Ok(EXIT_USAGE);
        }
    };
    invoke_program(
        program.as_program(),
        &entry,
        &options.program_arguments,
        json_output,
        stdout,
        stderr,
    )
}

fn execute_native_with_options(
    options: &Options,
    executable: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    execute_native(
        options.json,
        executable,
        &options.program_arguments,
        stdout,
        stderr,
    )
}

fn execute_native(
    json_output: bool,
    executable: &Path,
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let output = match ProcessCommand::new(executable).args(arguments).output() {
        Ok(output) => output,
        Err(error) => {
            emit_tool_error(
                json_output,
                stdout,
                stderr,
                "ArtifactLoadFailed",
                &format!("{}: {error}", executable.display()),
            )?;
            return Ok(EXIT_USAGE);
        }
    };
    if json_output {
        write_json_line(
            stdout,
            &json!({
                "schema_version": 1,
                "category": "native_execution",
                "status": if output.status.success() { "ok" } else { "failed" },
                "exit_code": output.status.code(),
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr),
            }),
        )?;
    } else {
        stdout.write_all(&output.stdout)?;
        stderr.write_all(&output.stderr)?;
    }
    Ok(if output.status.success() {
        EXIT_SUCCESS
    } else {
        EXIT_FAILURE
    })
}

fn invoke_program(
    program: &loom_mir::Program,
    entry: &str,
    arguments: &[String],
    json_output: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let Some(function_id) = program.exports.get(entry).copied() else {
        emit_tool_error(
            json_output,
            stdout,
            stderr,
            "UnknownEntry",
            &format!("no exported entry named `{entry}`"),
        )?;
        return Ok(EXIT_FAILURE);
    };
    let call_site = program
        .function(function_id)
        .map_or_else(|| Span::new(FileId(0), 0, 0), |function| function.span);
    match loom_interpreter::Interpreter::new(program)
        .with_process_arguments(arguments.to_vec())
        .invoke(function_id, Vec::new(), call_site)
    {
        Ok(value) => {
            if json_output {
                write_json_line(
                    stdout,
                    &json!({
                        "schema_version": 1,
                        "category": "run_result",
                        "entry": entry,
                        "value": value,
                    }),
                )?;
            } else {
                writeln!(stdout, "{}", value.summary())?;
            }
            Ok(EXIT_SUCCESS)
        }
        Err(failure) => {
            if json_output {
                write_json_line(
                    stdout,
                    &json!({
                        "schema_version": 1,
                        "category": "run_failure",
                        "entry": entry,
                        "failure": failure,
                    }),
                )?;
            } else {
                writeln!(stderr, "run `{entry}` failed: {failure:?}")?;
            }
            Ok(EXIT_FAILURE)
        }
    }
}

fn run_format(
    options: &Options,
    check: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let Some(snapshot) = load_snapshot(options, stdout, stderr)? else {
        return Ok(EXIT_USAGE);
    };
    let mut failed = false;
    let mut changed = Vec::new();
    for source in snapshot.sources().documents() {
        let Some(text) = source.text() else {
            failed = true;
            continue;
        };
        let formatted = format_source(source.id(), text);
        if !formatted.diagnostics.is_empty() {
            failed = true;
            emit_records(
                formatted.diagnostics.iter().filter_map(|diagnostic| {
                    DiagnosticRecord::from_diagnostic(diagnostic, snapshot.sources())
                }),
                options.json,
                stdout,
                stderr,
            )?;
            continue;
        }
        if formatted.changed_from(text) {
            changed.push(source.relative_path().to_owned());
            if !check {
                std::fs::write(source.absolute_path(), formatted.text.as_bytes()).map_err(
                    |error| {
                        io::Error::new(
                            error.kind(),
                            format!("{}: {error}", source.absolute_path().display()),
                        )
                    },
                )?;
            }
        }
    }
    if snapshot
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.code == "InvalidUtf8")
    {
        emit_records(
            snapshot
                .diagnostic_records()
                .into_iter()
                .filter(|diagnostic| diagnostic.code == "InvalidUtf8"),
            options.json,
            stdout,
            stderr,
        )?;
    }
    for path in &changed {
        if options.json {
            write_json_line(
                stdout,
                &json!({
                    "schema_version": 1,
                    "category": if check { "format_required" } else { "formatted" },
                    "path": path,
                }),
            )?;
        } else if check {
            writeln!(stderr, "would reformat {path}")?;
        } else {
            writeln!(stdout, "formatted {path}")?;
        }
    }
    if failed || (check && !changed.is_empty()) {
        Ok(EXIT_FAILURE)
    } else {
        Ok(EXIT_SUCCESS)
    }
}

fn load_snapshot(
    options: &Options,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<Option<AnalysisSnapshot>> {
    let Some(host) = open_analysis_host(options, false, stdout, stderr)? else {
        return Ok(None);
    };
    match host.snapshot() {
        Ok(snapshot) => Ok(Some(snapshot)),
        Err(error) => {
            emit_tool_error(
                options.json,
                stdout,
                stderr,
                "ProjectLoadFailed",
                &error.to_string(),
            )?;
            Ok(None)
        }
    }
}

#[allow(clippy::too_many_lines)]
fn load_compilation(
    options: &Options,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<Option<Compilation>> {
    let Some(host) = open_analysis_host(options, false, stdout, stderr)? else {
        return Ok(None);
    };
    let sources = match host.load_sources() {
        Ok(sources) => sources,
        Err(error) => {
            emit_tool_error(
                options.json,
                stdout,
                stderr,
                "ProjectLoadFailed",
                &error.to_string(),
            )?;
            return Ok(None);
        }
    };
    if options.no_cache {
        emit_cache_result(
            options.json,
            stdout,
            "source_parse",
            CacheStatus::Disabled,
            None,
        )?;
        emit_cache_result(
            options.json,
            stdout,
            "module_interface",
            CacheStatus::Disabled,
            None,
        )?;
        emit_cache_result(
            options.json,
            stdout,
            "checked_mir",
            CacheStatus::Disabled,
            None,
        )?;
        return Ok(Some(Compilation {
            data: CompilationData::Fresh(Box::new(host.snapshot_from_sources(sources))),
            cache: None,
            key: None,
        }));
    }

    let context = match cache_context(options.backend) {
        Ok(context) => context,
        Err(error) => {
            emit_tool_error(options.json, stdout, stderr, error.code(), error.message())?;
            return Ok(None);
        }
    };
    let key = PersistentCache::compilation_key(host.project(), &sources, &context);
    let cache = options.cache_dir.as_ref().map_or_else(
        || PersistentCache::for_project(host.project()),
        |path| PersistentCache::new(path.clone()),
    );
    if let CacheLookup::Hit(cached) = cache.load_compilation(&key) {
        emit_cache_result(
            options.json,
            stdout,
            "checked_mir",
            CacheStatus::Hit,
            Some(&key),
        )?;
        let (program, diagnostics) = cached.into_parts();
        return Ok(Some(Compilation {
            data: CompilationData::Cached(Box::new(CachedCompilationData {
                project: host.project().clone(),
                sources,
                program,
                diagnostics,
            })),
            cache: Some(cache),
            key: Some(key),
        }));
    }

    emit_cache_result(
        options.json,
        stdout,
        "checked_mir",
        CacheStatus::Miss,
        Some(&key),
    )?;
    let (snapshot, parse_stats) =
        host.snapshot_from_sources_with_parse_cache(sources, &cache, &context.compiler_version);
    emit_layer_cache_result(options.json, stdout, "source_parse", parse_stats)?;
    let interface_stats = sync_module_interfaces(
        &cache,
        &snapshot,
        &context.compiler_version,
        !snapshot.has_errors(),
    );
    emit_layer_cache_result(options.json, stdout, "module_interface", interface_stats)?;
    if !snapshot.has_errors()
        && let Ok(program) = snapshot.executable()
    {
        let diagnostics = snapshot.diagnostic_records();
        let _ = cache.store_compilation(&key, program, &diagnostics);
    }
    Ok(Some(Compilation {
        data: CompilationData::Fresh(Box::new(snapshot)),
        cache: Some(cache),
        key: Some(key),
    }))
}

fn cache_context(backend: Backend) -> Result<CacheContext, loom_codegen_llvm::CodegenError> {
    let build_profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let compiler_version = format!(
        "loomc-{}/source-{}/profile-{}/{}-{}",
        env!("CARGO_PKG_VERSION"),
        env!("LOOM_COMPILER_SOURCE_FINGERPRINT"),
        build_profile,
        loom_mir::INTERPRETED_ARTIFACT_FORMAT,
        loom_mir::INTERPRETED_ARTIFACT_VERSION
    );
    let context = match backend {
        Backend::Llvm => {
            let target = loom_codegen_llvm::native_target_identity()?;
            CacheContext {
                compiler_version,
                backend_version: format!(
                    "loom-codegen-llvm-{}",
                    loom_codegen_llvm::BACKEND_VERSION
                ),
                standard_library_version: "loom-core-inline-v1".to_owned(),
                runtime_abi_version: loom_codegen_llvm::NATIVE_RUNTIME_ABI.to_owned(),
                target_triple: target.triple,
                data_layout: target.data_layout,
                cpu_policy: format!("{};features={}", target.cpu_policy, target.cpu_features),
                optimization: format!("{};relocation={}", target.optimization, target.relocation),
                contract_mode: "checked".to_owned(),
            }
        }
        Backend::Interpreter => CacheContext {
            compiler_version,
            backend_version: format!("loom-interpreter-{}", loom_interpreter::BACKEND_VERSION),
            standard_library_version: "loom-core-inline-v1".to_owned(),
            runtime_abi_version: "loom-interpreter-value-v1".to_owned(),
            target_triple: "loom-portable-mir".to_owned(),
            data_layout: "loom-value-model-v1".to_owned(),
            cpu_policy: "portable".to_owned(),
            optimization: "validated-mir".to_owned(),
            contract_mode: "checked".to_owned(),
        },
    };
    Ok(context)
}

fn final_artifact_key(
    compilation: &Compilation,
    program: &loom_mir::Program,
    backend: Backend,
    mode: &str,
    entry: Option<&str>,
) -> Option<CacheKey> {
    let (parent, toolchain, runtime) = match backend {
        Backend::Llvm => (
            target_object_key(compilation, program, mode, entry)?,
            format!(
                "{};debug={}",
                loom_codegen_llvm::native_linker_identity().ok()?,
                loom_codegen_llvm::native_debug_tool_identity()
                    .ok()?
                    .unwrap_or_else(|| "embedded-elf-dwarf".to_owned())
            ),
            loom_codegen_llvm::native_runtime_identity(),
        ),
        Backend::Interpreter => (
            compilation.key()?.clone(),
            "loom-interpreted-artifact-writer-v2".to_owned(),
            "loom-interpreter-runtime-v1".to_owned(),
        ),
    };
    Some(PersistentCache::derived_key(
        &parent,
        &[
            ("layer", "final-artifact-v1"),
            ("mode", mode),
            ("entry", entry.unwrap_or("")),
            ("artifact-toolchain", &toolchain),
            ("runtime", &runtime),
        ],
    ))
}

fn library_artifact_key(compilation: &Compilation, target: &str) -> Option<CacheKey> {
    let version = loom_mir::INTERPRETED_ARTIFACT_VERSION.to_string();
    Some(PersistentCache::derived_key(
        compilation.key()?,
        &[
            ("layer", "portable-library-artifact-v1"),
            ("target", target),
            ("format", loom_mir::INTERPRETED_ARTIFACT_FORMAT),
            ("version", &version),
        ],
    ))
}

fn target_object_key(
    compilation: &Compilation,
    program: &loom_mir::Program,
    mode: &str,
    entry: Option<&str>,
) -> Option<CacheKey> {
    compilation.key()?;
    let base = match mode {
        "run" => loom_codegen_llvm::EmitOptions::run(entry?),
        "tests" => loom_codegen_llvm::EmitOptions::tests(),
        _ => return None,
    };
    let emit_options = emit_options_with_debug(compilation, &base);
    let fingerprint = loom_codegen_llvm::native_object_fingerprint(program, &emit_options).ok()?;
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    Some(PersistentCache::semantic_key(
        "loom-llvm-object-cache-v2",
        &[
            ("compiler-source", env!("LOOM_COMPILER_SOURCE_FINGERPRINT")),
            ("compiler-profile", profile),
            ("object-fingerprint", &fingerprint),
        ],
    ))
}

fn emit_options_with_debug(
    compilation: &Compilation,
    emit_options: &loom_codegen_llvm::EmitOptions,
) -> loom_codegen_llvm::EmitOptions {
    let debug_sources = compilation
        .sources()
        .documents()
        .iter()
        .filter_map(|source| {
            source.text().map(|text| {
                loom_codegen_llvm::DebugSource::new(source.id().0, source.relative_path(), text)
            })
        })
        .collect();
    emit_options.clone().with_debug_sources(debug_sources)
}

#[allow(clippy::too_many_arguments)]
fn emit_native_with_cache(
    compilation: &Compilation,
    program: &loom_mir::Program,
    output: &Path,
    emit_options: &loom_codegen_llvm::EmitOptions,
    mode: &str,
    entry: Option<&str>,
    options: &Options,
    stdout: &mut dyn Write,
) -> io::Result<Result<(), loom_codegen_llvm::CodegenError>> {
    let directory = tempfile::tempdir()?;
    let object = directory.path().join("loom-target.o");
    let key = target_object_key(compilation, program, mode, entry);
    let restored = if let (Some(cache), Some(key)) = (compilation.cache(), key.as_ref()) {
        match cache.load_target_object(key) {
            CacheLookup::Hit(bytes) if cache.materialize(&bytes, &object, false).is_ok() => {
                emit_cache_result(
                    options.json,
                    stdout,
                    "target_object",
                    CacheStatus::Hit,
                    Some(key),
                )?;
                true
            }
            CacheLookup::Hit(_) | CacheLookup::Miss => {
                emit_cache_result(
                    options.json,
                    stdout,
                    "target_object",
                    CacheStatus::Miss,
                    Some(key),
                )?;
                false
            }
        }
    } else {
        emit_cache_result(
            options.json,
            stdout,
            "target_object",
            CacheStatus::Disabled,
            None,
        )?;
        false
    };

    if !restored {
        let emit_options = emit_options_with_debug(compilation, emit_options);
        if let Err(error) = loom_codegen_llvm::emit_native_object(program, &object, &emit_options) {
            return Ok(Err(error));
        }
        if let (Some(cache), Some(key), Ok(bytes)) =
            (compilation.cache(), key.as_ref(), std::fs::read(&object))
        {
            let _ = cache.store_target_object(key, &bytes);
        }
    }
    if let Err(error) = loom_codegen_llvm::link_native_object(&object, output) {
        return Ok(Err(error));
    }
    Ok(loom_codegen_llvm::emit_native_debug_companion(output))
}

fn restore_cached_artifact(
    compilation: &Compilation,
    key: Option<&CacheKey>,
    output: &Path,
    executable: bool,
    options: &Options,
    stdout: &mut dyn Write,
) -> io::Result<Result<bool, String>> {
    let (Some(cache), Some(key)) = (compilation.cache(), key) else {
        emit_cache_result(
            options.json,
            stdout,
            "final_artifact",
            CacheStatus::Disabled,
            None,
        )?;
        return Ok(Ok(false));
    };
    let CacheLookup::Hit(bytes) = cache.load_artifact(key) else {
        emit_cache_result(
            options.json,
            stdout,
            "final_artifact",
            CacheStatus::Miss,
            Some(key),
        )?;
        return Ok(Ok(false));
    };
    let debug_companion = if executable {
        if let Some(path) = loom_codegen_llvm::native_debug_companion_path(output) {
            let CacheLookup::Hit(debug_bytes) = cache.load_debug_companion(key) else {
                emit_cache_result(
                    options.json,
                    stdout,
                    "final_artifact",
                    CacheStatus::Miss,
                    Some(key),
                )?;
                return Ok(Ok(false));
            };
            Some((path, debug_bytes))
        } else {
            None
        }
    } else {
        None
    };
    if let Err(error) = cache.materialize(&bytes, output, executable) {
        return Ok(Err(error.to_string()));
    }
    if let Some((path, bytes)) = debug_companion {
        if let Err(error) = cache.materialize(&bytes, &path, false) {
            return Ok(Err(error.to_string()));
        }
        if let Err(error) = loom_codegen_llvm::materialize_native_debug_metadata(output) {
            return Ok(Err(error.to_string()));
        }
    }
    emit_cache_result(
        options.json,
        stdout,
        "final_artifact",
        CacheStatus::Hit,
        Some(key),
    )?;
    Ok(Ok(true))
}

fn store_artifact_best_effort(compilation: &Compilation, key: Option<&CacheKey>, output: &Path) {
    let (Some(cache), Some(key), Ok(bytes)) = (compilation.cache(), key, std::fs::read(output))
    else {
        return;
    };
    let _ = cache.store_artifact(key, &bytes);
    if let Some(path) = loom_codegen_llvm::native_debug_companion_path(output)
        && let Ok(debug) = std::fs::read(path)
    {
        let _ = cache.store_debug_companion(key, &debug);
    }
}

fn emit_cache_result(
    json_output: bool,
    stdout: &mut dyn Write,
    layer: &str,
    status: CacheStatus,
    key: Option<&CacheKey>,
) -> io::Result<()> {
    if !json_output {
        return Ok(());
    }
    write_json_line(
        stdout,
        &json!({
            "schema_version": 1,
            "category": "cache_result",
            "layer": layer,
            "status": status.as_str(),
            "key": key.map(CacheKey::as_str),
        }),
    )
}

fn sync_module_interfaces(
    cache: &PersistentCache,
    snapshot: &AnalysisSnapshot,
    compiler_version: &str,
    store: bool,
) -> loom_driver::ParseCacheStats {
    let mut stats = loom_driver::ParseCacheStats::default();
    for interface in snapshot.module_interfaces() {
        let key = PersistentCache::module_interface_key(&interface, compiler_version);
        if cache.load_module_interface(&key, &interface).is_hit() {
            stats.hits += 1;
        } else {
            stats.misses += 1;
            if store {
                let _ = cache.store_module_interface(&key, &interface);
            }
        }
    }
    stats
}

fn emit_layer_cache_result(
    json_output: bool,
    stdout: &mut dyn Write,
    layer: &str,
    stats: loom_driver::ParseCacheStats,
) -> io::Result<()> {
    if !json_output {
        return Ok(());
    }
    write_json_line(
        stdout,
        &json!({
            "schema_version": 1,
            "category": "cache_result",
            "layer": layer,
            "status": if stats.is_full_hit() { "hit" } else { "miss" },
            "key": Value::Null,
            "hits": stats.hits,
            "misses": stats.misses,
        }),
    )
}

fn emit_build_result(options: &Options, stdout: &mut dyn Write, output: &Path) -> io::Result<()> {
    if options.json {
        write_json_line(
            stdout,
            &json!({
                "schema_version": 1,
                "category": "build_result",
                "status": "ok",
                "path": output,
            }),
        )
    } else {
        writeln!(stdout, "built {}", output.display())
    }
}

fn emit_source_diagnostics(
    compilation: &Compilation,
    json_output: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<bool> {
    emit_records(
        compilation.diagnostic_records(),
        json_output,
        stdout,
        stderr,
    )?;
    Ok(compilation.has_errors())
}

fn emit_records(
    records: impl IntoIterator<Item = DiagnosticRecord>,
    json_output: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    for record in records {
        if json_output {
            write_json_line(
                stdout,
                &serde_json::to_value(record).map_err(io::Error::other)?,
            )?;
        } else {
            writeln!(stderr, "{}", record.human())?;
        }
    }
    Ok(())
}

fn emit_unavailable(
    unavailable: &StageUnavailable,
    json_output: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    if json_output {
        write_json_line(
            stdout,
            &json!({
                "schema_version": 1,
                "category": "tool_error",
                "code": unavailable.code,
                "message": unavailable.message,
                "requested_stage": unavailable.requested,
                "completed_stage": unavailable.completed,
            }),
        )
    } else {
        writeln!(stderr, "error[{}]: {}", unavailable.code, unavailable)
    }
}

fn emit_target_error(
    options: &Options,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    error: &TargetSelectionError,
) -> io::Result<i32> {
    emit_tool_error(options.json, stdout, stderr, error.code, &error.message)?;
    Ok(EXIT_USAGE)
}

fn emit_tool_error(
    json_output: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    code: &str,
    message: &str,
) -> io::Result<()> {
    if json_output {
        write_json_line(
            stdout,
            &json!({
                "schema_version": 1,
                "category": "tool_error",
                "code": code,
                "message": message,
            }),
        )
    } else {
        writeln!(stderr, "error[{code}]: {message}")
    }
}

fn emit_summary(
    json_output: bool,
    stdout: &mut dyn Write,
    command: &str,
    files: usize,
) -> io::Result<()> {
    if json_output {
        write_json_line(
            stdout,
            &json!({
                "schema_version": 1,
                "category": "command_result",
                "command": command,
                "status": "ok",
                "files": files,
            }),
        )
    } else {
        writeln!(stdout, "{command} succeeded ({files} files)")
    }
}

fn write_json_line(writer: &mut dyn Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(io::Error::other)?;
    writer.write_all(b"\n")
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<ParsedArgs, String> {
    let mut strings = arguments
        .into_iter()
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| "arguments must be valid UTF-8".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let program_arguments = strings
        .iter()
        .position(|argument| argument == "--")
        .map_or_else(Vec::new, |separator| {
            let mut trailing = strings.split_off(separator);
            trailing.remove(0);
            trailing
        });
    if strings
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        return Ok(ParsedArgs::Help);
    }
    if strings
        .iter()
        .any(|argument| argument == "--version" || argument == "-V")
    {
        return Ok(ParsedArgs::Version);
    }
    let json = take_flag(&mut strings, "--json");
    let backend = match take_option(&mut strings, "--backend")?.as_deref() {
        None | Some("llvm") => Backend::Llvm,
        Some("interpreter") => Backend::Interpreter,
        Some(other) => {
            return Err(format!(
                "unknown backend `{other}`; expected `llvm` or `interpreter`"
            ));
        }
    };
    let features = parse_feature_list(take_option(&mut strings, "--features")?)?;
    let no_default_features = take_flag(&mut strings, "--no-default-features");
    let locked = take_flag(&mut strings, "--locked");
    let target = take_option(&mut strings, "--target")?;
    let no_cache = take_flag(&mut strings, "--no-cache");
    let cache_dir = take_option(&mut strings, "--cache-dir")?.map(PathBuf::from);
    if no_cache && cache_dir.is_some() {
        return Err("--no-cache and --cache-dir are mutually exclusive".to_owned());
    }
    let Some(command_name) = strings.first().cloned() else {
        return Err("missing command".to_owned());
    };
    strings.remove(0);
    let command = parse_command(&command_name, &mut strings, backend)?;
    let path = strings
        .first()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let options = Options {
        command,
        path,
        json,
        backend,
        target,
        no_cache,
        cache_dir,
        project: ProjectOptions {
            features,
            no_default_features,
            lock_mode: if locked {
                LockMode::Locked
            } else {
                LockMode::Use
            },
        },
        program_arguments,
    };
    validate_parsed_options(&options, &strings)?;
    Ok(ParsedArgs::Run(options))
}

fn parse_command(
    command_name: &str,
    arguments: &mut Vec<String>,
    backend: Backend,
) -> Result<Command, String> {
    let command = match command_name {
        "resolve" => Command::Resolve {
            refresh: take_flag(arguments, "--update"),
        },
        "check" => Command::Check,
        "build" => {
            let default_output = match backend {
                Backend::Llvm => DEFAULT_NATIVE_ARTIFACT,
                Backend::Interpreter => DEFAULT_INTERPRETED_ARTIFACT,
            };
            Command::Build {
                output: take_option(arguments, "--output")?
                    .map_or_else(|| PathBuf::from(default_output), PathBuf::from),
                entry: take_option(arguments, "--entry")?,
            }
        }
        "test" => Command::Test,
        "run" => {
            let entry = take_option(arguments, "--entry")?;
            let artifact = take_option(arguments, "--artifact")?.map(PathBuf::from);
            Command::Run { entry, artifact }
        }
        "fmt" => Command::Format {
            check: take_flag(arguments, "--check"),
        },
        other => return Err(format!("unknown command `{other}`")),
    };
    Ok(command)
}

fn validate_parsed_options(options: &Options, remaining: &[String]) -> Result<(), String> {
    let command = &options.command;
    if !options.program_arguments.is_empty() && !matches!(command, Command::Run { .. }) {
        return Err("program arguments after `--` are only valid for run".to_owned());
    }
    if let Some(flag) = remaining.iter().find(|argument| argument.starts_with('-')) {
        return Err(format!("unknown option `{flag}`"));
    }
    if remaining.len() > 1 {
        return Err("expected at most one PATH".to_owned());
    }
    if options.target.is_some()
        && matches!(
            &command,
            Command::Build { entry: Some(_), .. }
                | Command::Run {
                    entry: Some(_),
                    artifact: None
                }
        )
    {
        return Err("--target and --entry are mutually exclusive".to_owned());
    }
    let resolution_options_used = !options.project.features.is_empty()
        || options.project.no_default_features
        || options.project.lock_mode == LockMode::Locked;
    if resolution_options_used
        && matches!(
            command,
            Command::Format { .. }
                | Command::Run {
                    artifact: Some(_),
                    ..
                }
        )
    {
        return Err(
            "feature and lock options are only valid for resolve or source commands".to_owned(),
        );
    }
    if options.project.lock_mode == LockMode::Locked
        && matches!(command, Command::Resolve { refresh: true })
    {
        return Err("--locked and resolve --update are mutually exclusive".to_owned());
    }
    if options.target.is_some()
        && matches!(
            &command,
            Command::Resolve { .. }
                | Command::Format { .. }
                | Command::Run {
                    artifact: Some(_),
                    ..
                }
        )
    {
        return Err("--target is only valid for source check/build/test/run".to_owned());
    }
    if (options.no_cache || options.cache_dir.is_some())
        && matches!(
            &command,
            Command::Resolve { .. }
                | Command::Format { .. }
                | Command::Run {
                    artifact: Some(_),
                    ..
                }
        )
    {
        return Err("cache options are only valid for source check/build/test/run".to_owned());
    }
    if matches!(
        command,
        Command::Run {
            artifact: Some(_),
            ..
        }
    ) && !remaining.is_empty()
    {
        return Err("run --artifact does not also accept a source PATH".to_owned());
    }
    Ok(())
}

fn parse_feature_list(value: Option<String>) -> Result<BTreeSet<String>, String> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    if value.is_empty() {
        return Err("--features requires a non-empty comma-separated list".to_owned());
    }
    let features = value.split(',').map(str::to_owned).collect::<BTreeSet<_>>();
    if features.iter().any(String::is_empty) {
        return Err("--features cannot contain an empty feature name".to_owned());
    }
    Ok(features)
}

fn take_flag(arguments: &mut Vec<String>, flag: &str) -> bool {
    if let Some(index) = arguments.iter().position(|argument| argument == flag) {
        arguments.remove(index);
        true
    } else {
        false
    }
}

fn take_option(arguments: &mut Vec<String>, option: &str) -> Result<Option<String>, String> {
    if let Some(index) = arguments.iter().position(|argument| argument == option) {
        arguments.remove(index);
        if index >= arguments.len() {
            return Err(format!("{option} requires a value"));
        }
        return Ok(Some(arguments.remove(index)));
    }
    let prefix = format!("{option}=");
    if let Some(index) = arguments
        .iter()
        .position(|argument| argument.starts_with(&prefix))
    {
        let argument = arguments.remove(index);
        return Ok(Some(argument[prefix.len()..].to_owned()));
    }
    Ok(None)
}
