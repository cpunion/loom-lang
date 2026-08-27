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
    publish_registry_package,
};
use loom_interpreter::TestStatus;
use serde_json::{Value, json};

const USAGE: &str = "usage: loomc [--json] [--backend llvm|interpreter] [--release] [--features A,B] [--no-default-features] [--locked] [--offline] [--no-cache | --cache-dir DIR] [--runtime-bundle DIR] [--linker PROGRAM] <resolve|publish|runtime|check|build|test|run|debug|fmt|cache> [options] [PATH]\n\
    resolve [--update] [PATH] resolve dependencies and materialize loom.lock\n\
    publish --registry NAME [PATH] publish a package to a configured registry\n\
    runtime pack --archive FILE --output DIR pack a validated host runtime bundle\n\
    check [--target NAME] [PATH] parse, lower, and type-check a project\n\
    build [--target NAME | --entry NAME] [--target-triple TRIPLE] [--runtime-bundle DIR] [--linker PROGRAM] [--emit executable|object] [--output FILE] [PATH] build an executable, object, or portable library\n\
    test [--target NAME] [PATH] compile and execute ordinary test fn declarations\n\
    run [--target NAME | --entry NAME] [PATH] [-- ARGS...] compile and execute an exported function\n\
    run --artifact FILE [-- ARGS...] execute a previously built artifact\n\
    debug [--target NAME | --entry NAME] [--debugger PROGRAM] [PATH] [-- ARGS...] build with source info and launch LLDB/GDB\n\
    fmt [--check] [PATH]     format .loom files (default PATH is .)\n\
    cache <stat|prune> [PATH] inspect or explicitly prune the versioned project cache";

const DEFAULT_NATIVE_ARTIFACT: &str = "target/loom/program";
const DEFAULT_OBJECT_ARTIFACT: &str = "target/loom/program";
const DEFAULT_INTERPRETED_ARTIFACT: &str = "target/loom/program.loomi";
const NATIVE_FAULT_FORMAT_ENV: &str = "LOOM_FAULT_FORMAT";
const NATIVE_FAULT_JSON_PREFIX: &str = "LOOM_FAULT_JSON_V1:";
const RUNTIME_BUNDLE_ENV: &str = "LOOM_RUNTIME_BUNDLE";
const LINKER_ENV: &str = "LOOM_CC";
const LLVM_OBJECT_CACHE_DOMAIN: &str = "loom-llvm-object-cache-v8";
#[cfg(any(target_os = "macos", target_os = "windows"))]
const DEFAULT_DEBUGGER: &str = "lldb";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const DEFAULT_DEBUGGER: &str = "gdb";

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
    Publish {
        registry: String,
    },
    RuntimePack {
        archive: PathBuf,
        output: PathBuf,
    },
    Check,
    Build {
        output: PathBuf,
        entry: Option<String>,
        emit: BuildEmit,
    },
    Test,
    Run {
        entry: Option<String>,
        artifact: Option<PathBuf>,
    },
    Debug {
        entry: Option<String>,
        debugger: PathBuf,
    },
    Format {
        check: bool,
    },
    Cache {
        prune: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuildEmit {
    Executable,
    Object,
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
    target_triple: Option<String>,
    runtime_bundle: Option<PathBuf>,
    linker: Option<PathBuf>,
    optimization: loom_codegen_llvm::OptimizationProfile,
    program_arguments: Vec<String>,
}

struct NativeLinkPlan {
    bundle: Box<loom_codegen_llvm::RuntimeBundle>,
    linker: loom_codegen_llvm::RuntimeLinker,
}

enum NativePipelineError {
    Preparation(loom_codegen_llvm::NativePreparationError),
    Configuration(NativeConfigurationError),
    Codegen(loom_codegen_llvm::CodegenError),
}

#[derive(Debug)]
struct NativeConfigurationError {
    code: &'static str,
    message: String,
}

impl NativeConfigurationError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<loom_codegen_llvm::CodegenError> for NativeConfigurationError {
    fn from(error: loom_codegen_llvm::CodegenError) -> Self {
        Self::new(error.code(), error.message())
    }
}

impl NativePipelineError {
    fn code(&self) -> &'static str {
        match self {
            Self::Preparation(error) => error.code(),
            Self::Configuration(error) => error.code,
            Self::Codegen(error) => error.code(),
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Preparation(error) => error.message(),
            Self::Configuration(error) => &error.message,
            Self::Codegen(error) => error.message(),
        }
    }

    const fn exit_status(&self) -> i32 {
        match self {
            Self::Preparation(error) => match error.kind() {
                loom_codegen_llvm::NativePreparationErrorKind::InvalidRoot
                | loom_codegen_llvm::NativePreparationErrorKind::Resource => EXIT_FAILURE,
                loom_codegen_llvm::NativePreparationErrorKind::Target => EXIT_USAGE,
                loom_codegen_llvm::NativePreparationErrorKind::Defect => EXIT_DEFECT,
            },
            Self::Configuration(_) => EXIT_USAGE,
            Self::Codegen(_) => EXIT_DEFECT,
        }
    }
}

impl NativeLinkPlan {
    fn link(&self, object: &Path, output: &Path) -> Result<(), loom_codegen_llvm::CodegenError> {
        loom_codegen_llvm::link_object_with_runtime_bundle(
            object,
            output,
            &self.bundle,
            &self.linker,
        )
    }
}

enum ParsedArgs {
    Run(Box<Options>),
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
    program: loom_mir::CheckedProgram,
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

    fn executable(&self) -> Result<&loom_mir::CheckedProgram, StageUnavailable> {
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

fn validate_entry_point(
    program: &loom_mir::CheckedProgram,
    entry: &str,
) -> Result<(), TargetSelectionError> {
    let function = program
        .exports
        .get(entry)
        .copied()
        .and_then(|function| program.function(function))
        .ok_or_else(|| TargetSelectionError {
            code: "UnknownEntry",
            message: format!("no public root-package entry named `{entry}`"),
        })?;
    let valid = function.type_parameters == 0
        && function.params.is_empty()
        && function.witness_params.is_empty()
        && function.receiver.is_none()
        && function.return_ty == loom_mir::Type::Unit;
    if valid {
        return Ok(());
    }
    Err(TargetSelectionError {
        code: "InvalidEntrySignature",
        message: format!(
            "entry `{entry}` must be a public root-package function with no receiver, parameters, type parameters, or witnesses, returning `Unit`"
        ),
    })
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
        ParsedArgs::Run(options) => *options,
    };

    match &options.command {
        Command::Resolve { refresh } => run_resolve(&options, *refresh, stdout, stderr),
        Command::Publish { registry } => run_publish(&options, registry, stdout, stderr),
        Command::RuntimePack { archive, output } => {
            run_runtime_pack(&options, archive, output, stdout, stderr)
        }
        Command::Format { check } => run_format(&options, *check, stdout, stderr),
        Command::Cache { prune } => run_cache(&options, *prune, stdout, stderr),
        Command::Check => run_check(&options, stdout, stderr),
        Command::Build {
            output,
            entry,
            emit,
        } => run_build(&options, output, entry.as_deref(), *emit, stdout, stderr),
        Command::Test => run_test(&options, stdout, stderr),
        Command::Run { entry, artifact } => {
            if let Some(artifact) = artifact {
                run_artifact(&options, artifact, entry.as_deref(), stdout, stderr)
            } else {
                run_program(&options, entry.as_deref(), stdout, stderr)
            }
        }
        Command::Debug { entry, debugger } => {
            run_debug(&options, entry.as_deref(), debugger, stdout, stderr)
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
                    error.code(),
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
                "schema_version": loom_codegen_llvm::RUNTIME_BUNDLE_SCHEMA_VERSION,
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

fn run_publish(
    options: &Options,
    registry: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let published = match publish_registry_package(&options.path, registry) {
        Ok(published) => published,
        Err(error) => {
            emit_tool_error(
                options.json,
                stdout,
                stderr,
                "RegistryPublishFailed",
                &error.to_string(),
            )?;
            return Ok(EXIT_USAGE);
        }
    };
    if options.json {
        write_json_line(
            stdout,
            &json!({
                "schema_version": 1,
                "category": "registry_publish",
                "status": "ok",
                "registry": published.registry,
                "package": published.package,
                "version": published.version,
                "sha256": published.sha256,
                "endpoint": published.endpoint,
            }),
        )?;
    } else {
        writeln!(
            stdout,
            "published {}@{} to {} ({})",
            published.package, published.version, published.registry, published.sha256
        )?;
    }
    Ok(EXIT_SUCCESS)
}

fn run_runtime_pack(
    options: &Options,
    archive: &Path,
    output: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let packed = match loom_codegen_llvm::pack_native_runtime_bundle(archive, output) {
        Ok(packed) => packed,
        Err(error) => {
            emit_tool_error(options.json, stdout, stderr, error.code(), error.message())?;
            return Ok(EXIT_USAGE);
        }
    };
    if options.json {
        write_json_line(
            stdout,
            &json!({
                "schema_version": 1,
                "category": "runtime_bundle_pack",
                "status": "ok",
                "root": packed.root,
                "manifest": packed.manifest,
                "archive": packed.archive,
                "target_triple": packed.target_triple,
                "data_layout": packed.data_layout,
                "runtime_cpu": packed.runtime_cpu,
                "runtime_cpu_features": packed.runtime_cpu_features,
                "runtime_abi": packed.runtime_abi,
                "archive_sha256": packed.archive_sha256,
            }),
        )?;
    } else {
        writeln!(
            stdout,
            "packed runtime bundle for {} into {}",
            packed.target_triple,
            packed.root.display()
        )?;
    }
    Ok(EXIT_SUCCESS)
}

#[allow(clippy::too_many_lines)]
fn run_build(
    options: &Options,
    output: &Path,
    explicit_entry: Option<&str>,
    emit: BuildEmit,
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
    let output = if matches!(target, BuildTarget::Binary(_)) && options.backend == Backend::Llvm {
        let kind = if emit == BuildEmit::Object {
            loom_codegen_llvm::NativeArtifactKind::Object
        } else {
            loom_codegen_llvm::NativeArtifactKind::Executable
        };
        loom_codegen_llvm::native_artifact_path(output, options.target_triple.as_deref(), kind)
    } else {
        output.to_path_buf()
    };
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
        if emit == BuildEmit::Object
            || options.target_triple.is_some()
            || options.runtime_bundle.is_some()
            || options.linker.is_some()
            || options.optimization == loom_codegen_llvm::OptimizationProfile::Release
        {
            emit_tool_error(
                options.json,
                stdout,
                stderr,
                "LibraryTargetIsPortable",
                "library targets already emit portable checked MIR and do not accept --release, --emit object, --target-triple, or runtime link options",
            )?;
            return Ok(EXIT_USAGE);
        }
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
    if let Err(error) = validate_entry_point(program, &entry) {
        return emit_entry_error(options, stdout, stderr, &error);
    }
    let emit_options =
        configured_emit_options(options, loom_codegen_llvm::EmitOptions::run(&entry));
    if emit == BuildEmit::Object {
        if let Err(error) = emit_object_with_cache(
            &compilation,
            program,
            &output,
            &emit_options,
            options,
            stdout,
        )? {
            emit_tool_error(options.json, stdout, stderr, error.code(), error.message())?;
            return Ok(error.exit_status());
        }
        emit_build_result(options, stdout, &output)?;
        return Ok(EXIT_SUCCESS);
    }
    let artifact_key = final_artifact_key(&compilation, options.backend, "run", Some(&entry));
    match restore_cached_artifact(
        &compilation,
        artifact_key.as_ref(),
        &output,
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
                &emit_options,
                loom_codegen_llvm::NativeRoutePolicy::Automatic,
                options,
                stdout,
            )? {
                emit_tool_error(options.json, stdout, stderr, error.code(), error.message())?;
                return Ok(error.exit_status());
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
    program: &loom_mir::CheckedProgram,
    target: &str,
    output: &Path,
    options: &Options,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let artifact_key = library_artifact_key(compilation, target);
    match restore_cached_artifact(compilation, artifact_key.as_ref(), output, options, stdout)? {
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
    let bytes = match loom_driver::encode_library_artifact(
        compilation.project(),
        compilation.sources(),
        program,
    ) {
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
    if let Ok(program) = compilation.executable() {
        for target in compilation
            .project()
            .targets()
            .iter()
            .filter(|target| target.kind() == TargetKind::Bin)
        {
            let entry = target
                .entry()
                .expect("validated binary target has an entry");
            if let Err(error) = validate_entry_point(program, entry) {
                return emit_entry_error(options, stdout, stderr, &error);
            }
        }
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
        let executable = loom_codegen_llvm::native_artifact_path(
            directory.path().join("loom-tests"),
            None,
            loom_codegen_llvm::NativeArtifactKind::Executable,
        );
        let emit_options =
            configured_emit_options(options, loom_codegen_llvm::EmitOptions::tests());
        let artifact_key = final_artifact_key(&compilation, options.backend, "tests", None);
        match restore_cached_artifact(
            &compilation,
            artifact_key.as_ref(),
            &executable,
            options,
            stdout,
        )? {
            Ok(true) => {
                return execute_native_with_options(options, &executable, None, stdout, stderr);
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
            &emit_options,
            loom_codegen_llvm::NativeRoutePolicy::Automatic,
            options,
            stdout,
        )? {
            emit_tool_error(options.json, stdout, stderr, error.code(), error.message())?;
            return Ok(error.exit_status());
        }
        store_artifact_best_effort(&compilation, artifact_key.as_ref(), &executable);
        return execute_native_with_options(options, &executable, None, stdout, stderr);
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
    if let Err(error) = validate_entry_point(program, &entry) {
        return emit_entry_error(options, stdout, stderr, &error);
    }
    match options.backend {
        Backend::Llvm => {
            let directory = tempfile::tempdir()?;
            let executable = loom_codegen_llvm::native_artifact_path(
                directory.path().join("loom-program"),
                None,
                loom_codegen_llvm::NativeArtifactKind::Executable,
            );
            let emit_options =
                configured_emit_options(options, loom_codegen_llvm::EmitOptions::run(&entry));
            let artifact_key =
                final_artifact_key(&compilation, options.backend, "run", Some(&entry));
            match restore_cached_artifact(
                &compilation,
                artifact_key.as_ref(),
                &executable,
                options,
                stdout,
            )? {
                Ok(true) => {
                    return execute_native_with_options(
                        options,
                        &executable,
                        Some(&entry),
                        stdout,
                        stderr,
                    );
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
                &emit_options,
                loom_codegen_llvm::NativeRoutePolicy::Automatic,
                options,
                stdout,
            )? {
                emit_tool_error(options.json, stdout, stderr, error.code(), error.message())?;
                return Ok(error.exit_status());
            }
            store_artifact_best_effort(&compilation, artifact_key.as_ref(), &executable);
            execute_native_with_options(options, &executable, Some(&entry), stdout, stderr)
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

fn run_debug(
    options: &Options,
    explicit_entry: Option<&str>,
    debugger: &Path,
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
    if let Err(error) = validate_entry_point(program, &entry) {
        return emit_entry_error(options, stdout, stderr, &error);
    }

    let directory = tempfile::tempdir()?;
    let executable = loom_codegen_llvm::native_artifact_path(
        directory.path().join("loom-debug-program"),
        None,
        loom_codegen_llvm::NativeArtifactKind::Executable,
    );
    let emit_options =
        configured_emit_options(options, loom_codegen_llvm::EmitOptions::run(&entry));
    let artifact_key = final_artifact_key(&compilation, options.backend, "debug", Some(&entry));
    let restored = match restore_cached_artifact(
        &compilation,
        artifact_key.as_ref(),
        &executable,
        options,
        stdout,
    )? {
        Ok(restored) => restored,
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
    };
    if !restored {
        if let Err(error) = emit_native_with_cache(
            &compilation,
            program,
            &executable,
            &emit_options,
            loom_codegen_llvm::NativeRoutePolicy::Automatic,
            options,
            stdout,
        )? {
            emit_tool_error(options.json, stdout, stderr, error.code(), error.message())?;
            return Ok(error.exit_status());
        }
        store_artifact_best_effort(&compilation, artifact_key.as_ref(), &executable);
    }

    writeln!(
        stdout,
        "debugging {} with {}",
        executable.display(),
        debugger.display()
    )?;
    stdout.flush()?;
    stderr.flush()?;
    launch_debugger(
        debugger,
        &executable,
        compilation.project().root(),
        &options.program_arguments,
        stdout,
        stderr,
    )
}

fn launch_debugger(
    debugger: &Path,
    executable: &Path,
    project_root: &Path,
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let debugger_name = debugger
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut command = ProcessCommand::new(debugger);
    command.current_dir(project_root);
    if debugger_name == "gdb" || debugger_name.ends_with("-gdb") {
        command.arg("--args").arg(executable).args(arguments);
    } else {
        // LLDB uses this form directly. Custom debugger wrappers intentionally
        // receive the same stable contract: PROGRAM EXECUTABLE -- ARGS...
        command.arg(executable).arg("--").args(arguments);
    }
    let status = match command.status() {
        Ok(status) => status,
        Err(error) => {
            emit_tool_error(
                false,
                stdout,
                stderr,
                "DebuggerLaunchFailed",
                &format!("{}: {error}", debugger.display()),
            )?;
            return Ok(EXIT_USAGE);
        }
    };
    Ok(if status.success() {
        EXIT_SUCCESS
    } else {
        EXIT_FAILURE
    })
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
        return execute_native_with_options(options, artifact, None, stdout, stderr);
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
            let code = match &error {
                loom_mir::ArtifactError::VersionMismatch { .. } => "ArtifactVersionMismatch",
                loom_mir::ArtifactError::LanguageVersionMismatch { .. } => {
                    "ArtifactLanguageVersionMismatch"
                }
                _ => "ArtifactLoadFailed",
            };
            emit_tool_error(json_output, stdout, stderr, code, &error.to_string())?;
            return Ok(EXIT_USAGE);
        }
    };
    invoke_program(
        &program,
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
    entry: Option<&str>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    execute_native(
        options.json,
        executable,
        &options.program_arguments,
        entry,
        stdout,
        stderr,
    )
}

fn execute_native(
    json_output: bool,
    executable: &Path,
    arguments: &[String],
    entry: Option<&str>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let mut command = ProcessCommand::new(executable);
    command.args(arguments);
    if json_output {
        command.env(NATIVE_FAULT_FORMAT_ENV, "json");
    }
    let output = match command.output() {
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
        let (failure, child_stderr) = extract_native_failure(&output.stderr);
        if let Some(failure) = failure {
            write_json_line(
                stdout,
                &json!({
                    "schema_version": 1,
                    "category": "run_failure",
                    "entry": entry,
                    "failure": failure,
                }),
            )?;
            return Ok(EXIT_FAILURE);
        }
        write_json_line(
            stdout,
            &json!({
                "schema_version": 1,
                "category": "native_execution",
                "status": if output.status.success() { "ok" } else { "failed" },
                "exit_code": output.status.code(),
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": child_stderr,
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

fn extract_native_failure(stderr: &[u8]) -> (Option<Value>, String) {
    let text = String::from_utf8_lossy(stderr);
    let mut failure = None;
    let mut retained = String::new();
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let parsed = content
            .strip_prefix(NATIVE_FAULT_JSON_PREFIX)
            .and_then(|payload| serde_json::from_str::<Value>(payload).ok());
        if let Some(parsed) = parsed {
            failure = Some(parsed);
        } else {
            retained.push_str(line);
        }
    }
    (failure, retained)
}

fn invoke_program(
    program: &loom_mir::CheckedProgram,
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
        if !source.is_root_package() {
            continue;
        }
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
                Some(snapshot.sources()),
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
            Some(snapshot.sources()),
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

fn run_cache(
    options: &Options,
    prune: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let cache = if let Some(root) = &options.cache_dir {
        PersistentCache::new(root)
    } else {
        let project = match ProjectGraph::load_with_options(&options.path, &options.project) {
            Ok(project) => project,
            Err(error) => {
                emit_tool_error(
                    options.json,
                    stdout,
                    stderr,
                    error.code(),
                    &error.to_string(),
                )?;
                return Ok(EXIT_USAGE);
            }
        };
        PersistentCache::for_project(&project)
    };
    if prune {
        let report = match cache.prune() {
            Ok(report) => report,
            Err(error) => {
                emit_tool_error(
                    options.json,
                    stdout,
                    stderr,
                    "CachePruneFailed",
                    &error.to_string(),
                )?;
                return Ok(EXIT_USAGE);
            }
        };
        if options.json {
            return write_json_line(
                stdout,
                &json!({
                    "schema_version": 1,
                    "category": "cache_prune",
                    "root": cache.root(),
                    "report": report,
                }),
            )
            .map(|()| EXIT_SUCCESS);
        }
        writeln!(
            stdout,
            "pruned {} refs and {} blobs ({} bytes) from {}",
            report.invalid_references_removed,
            report.blobs_removed,
            report.bytes_reclaimed,
            cache.root().display()
        )?;
        return Ok(EXIT_SUCCESS);
    }
    let stats = match cache.stats() {
        Ok(stats) => stats,
        Err(error) => {
            emit_tool_error(
                options.json,
                stdout,
                stderr,
                "CacheStatFailed",
                &error.to_string(),
            )?;
            return Ok(EXIT_USAGE);
        }
    };
    if options.json {
        write_json_line(
            stdout,
            &json!({
                "schema_version": 1,
                "category": "cache_stat",
                "root": cache.root(),
                "stats": stats,
            }),
        )?;
    } else {
        writeln!(
            stdout,
            "cache {}: {} refs ({} invalid), {} blobs, {} bytes, {} reclaimable bytes",
            cache.root().display(),
            stats.references,
            stats.invalid_references,
            stats.blobs,
            stats.bytes,
            stats.reclaimable_bytes
        )?;
    }
    Ok(EXIT_SUCCESS)
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
                error.code(),
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
                error.code(),
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

    let context = cache_context(host.project().language_version());
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
        host.snapshot_from_sources_with_parse_cache(sources, &cache, &context.frontend_identity);
    emit_layer_cache_result(options.json, stdout, "source_parse", parse_stats)?;
    let interface_stats = sync_module_interfaces(
        &cache,
        &snapshot,
        &context.frontend_identity,
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

fn cache_context(language_version: &str) -> CacheContext {
    let frontend_build = env!("LOOM_FRONTEND_BUILD_FINGERPRINT");
    let frontend_identity = format!(
        "loom-frontend-{}/build-{frontend_build}/{}-{}",
        env!("CARGO_PKG_VERSION"),
        loom_mir::INTERPRETED_ARTIFACT_FORMAT,
        loom_mir::INTERPRETED_ARTIFACT_VERSION
    );
    CacheContext {
        language_version: language_version.to_owned(),
        frontend_identity,
        standard_library_identity: format!("loom-embedded-builtins-v2/build-{frontend_build}"),
        contract_mode: "checked".to_owned(),
    }
}

fn final_artifact_key(
    compilation: &Compilation,
    backend: Backend,
    mode: &str,
    entry: Option<&str>,
) -> Option<CacheKey> {
    if backend == Backend::Llvm {
        // Native linking is intentionally outside persistent caching until a
        // hermetic link bundle identifies every linker child, SDK/sysroot,
        // CRT/system library and debug companion through one validated link
        // plan. Publishing a multi-file debug companion is not filesystem-
        // atomic across every platform.
        return None;
    }
    let parent = compilation.key()?.clone();
    Some(PersistentCache::derived_key(
        &parent,
        &[
            ("layer", "final-artifact-v2"),
            ("mode", mode),
            ("entry", entry.unwrap_or("")),
            ("artifact-toolchain", "loom-interpreted-artifact-writer-v2"),
            ("runtime", "loom-interpreter-runtime-v1"),
        ],
    ))
}

fn library_artifact_key(compilation: &Compilation, target: &str) -> Option<CacheKey> {
    let version = loom_driver::LIBRARY_ARTIFACT_VERSION.to_string();
    Some(PersistentCache::derived_key(
        compilation.key()?,
        &[
            ("layer", "portable-library-artifact-v2"),
            ("target", target),
            ("format", loom_driver::LIBRARY_ARTIFACT_FORMAT),
            ("version", &version),
        ],
    ))
}

fn target_object_key(
    compilation: &Compilation,
    prepared: &loom_codegen_llvm::PreparedNativeObject<'_>,
    cacheable: bool,
) -> Result<Option<CacheKey>, loom_codegen_llvm::CodegenError> {
    if compilation.key().is_none() || !cacheable {
        return Ok(None);
    }
    let fingerprint = loom_codegen_llvm::prepared_native_object_fingerprint(prepared)?;
    Ok(Some(PersistentCache::semantic_key(
        LLVM_OBJECT_CACHE_DOMAIN,
        &[("object-fingerprint", &fingerprint)],
    )))
}

fn configured_emit_options(
    options: &Options,
    emit_options: loom_codegen_llvm::EmitOptions,
) -> loom_codegen_llvm::EmitOptions {
    emit_options
        .with_target_triple(options.target_triple.clone())
        .with_optimization(options.optimization)
}

fn prepare_native_link_plan(
    options: &Options,
    _emit_options: &loom_codegen_llvm::EmitOptions,
    prepared: &loom_codegen_llvm::PreparedNativeObject<'_>,
) -> Result<NativeLinkPlan, NativeConfigurationError> {
    let target = loom_codegen_llvm::prepared_native_target_identity(prepared);
    let host = loom_codegen_llvm::native_target_identity()?;
    if target.triple != host.triple && options.linker.is_none() {
        return Err(NativeConfigurationError::new(
            "CrossLinkUnavailable",
            "cross-target executable linking requires an explicit --linker PROGRAM",
        ));
    }
    let bundle_path = resolve_runtime_bundle_path(options.runtime_bundle.as_deref())?;
    let linker_path = options
        .linker
        .clone()
        .or_else(|| std::env::var_os(LINKER_ENV).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("clang"));
    Ok(NativeLinkPlan {
        bundle: Box::new(loom_codegen_llvm::RuntimeBundle::load(bundle_path, target)?),
        linker: loom_codegen_llvm::RuntimeLinker::load(linker_path)?,
    })
}

fn resolve_runtime_bundle_path(
    explicit: Option<&Path>,
) -> Result<PathBuf, NativeConfigurationError> {
    if let Some(configured) =
        configured_runtime_bundle_path(explicit, std::env::var_os(RUNTIME_BUNDLE_ENV).as_deref())
    {
        return Ok(configured);
    }
    let executable = std::env::current_exe().map_err(|error| {
        NativeConfigurationError::new(
            "RuntimeBundleUnavailable",
            format!("cannot locate the running compiler: {error}"),
        )
    })?;
    let real_executable = std::fs::canonicalize(&executable).map_err(|error| {
        NativeConfigurationError::new(
            "RuntimeBundleUnavailable",
            format!(
                "cannot resolve compiler executable {}: {error}",
                executable.display()
            ),
        )
    })?;
    adjacent_runtime_bundle_path(&real_executable)
}

fn configured_runtime_bundle_path(
    explicit: Option<&Path>,
    environment: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| environment.map(PathBuf::from))
}

fn adjacent_runtime_bundle_path(
    real_executable: &Path,
) -> Result<PathBuf, NativeConfigurationError> {
    let parent = real_executable.parent().ok_or_else(|| {
        NativeConfigurationError::new(
            "RuntimeBundleUnavailable",
            "resolved compiler executable has no parent directory",
        )
    })?;
    Ok(parent.join("runtime"))
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

fn emit_native_with_cache(
    compilation: &Compilation,
    program: &loom_mir::CheckedProgram,
    output: &Path,
    emit_options: &loom_codegen_llvm::EmitOptions,
    policy: loom_codegen_llvm::NativeRoutePolicy,
    options: &Options,
    stdout: &mut dyn Write,
) -> io::Result<Result<(), NativePipelineError>> {
    let complete_options = emit_options_with_debug(compilation, emit_options);
    let cacheable = complete_options.emit_ir.is_none();
    let prepared = match loom_codegen_llvm::prepare_native_object(program, complete_options, policy)
    {
        Ok(prepared) => prepared,
        Err(error) => return Ok(Err(NativePipelineError::Preparation(error))),
    };
    let native_link = match prepare_native_link_plan(options, emit_options, &prepared) {
        Ok(link) => link,
        Err(error) => return Ok(Err(NativePipelineError::Configuration(error))),
    };
    let directory = tempfile::tempdir()?;
    let object = loom_codegen_llvm::native_artifact_path(
        directory.path().join("loom-target"),
        Some(
            loom_codegen_llvm::prepared_native_target_identity(&prepared)
                .triple
                .as_str(),
        ),
        loom_codegen_llvm::NativeArtifactKind::Object,
    );
    if let Err(error) = emit_prepared_object_with_cache(
        compilation,
        &prepared,
        &object,
        cacheable,
        options,
        stdout,
    )? {
        return Ok(Err(error));
    }
    let linked = native_link.link(&object, output);
    if let Err(error) = linked {
        return Ok(Err(NativePipelineError::Codegen(error)));
    }
    if loom_codegen_llvm::is_native_target(emit_options.target_triple.as_deref()) {
        Ok(loom_codegen_llvm::emit_native_debug_companion(output)
            .map_err(NativePipelineError::Codegen))
    } else {
        Ok(Ok(()))
    }
}

fn emit_object_with_cache(
    compilation: &Compilation,
    program: &loom_mir::CheckedProgram,
    object: &Path,
    emit_options: &loom_codegen_llvm::EmitOptions,
    options: &Options,
    stdout: &mut dyn Write,
) -> io::Result<Result<(), NativePipelineError>> {
    let complete_options = emit_options_with_debug(compilation, emit_options);
    let cacheable = complete_options.emit_ir.is_none();
    let prepared = match loom_codegen_llvm::prepare_native_object(
        program,
        complete_options,
        loom_codegen_llvm::NativeRoutePolicy::Automatic,
    ) {
        Ok(prepared) => prepared,
        Err(error) => return Ok(Err(NativePipelineError::Preparation(error))),
    };
    emit_prepared_object_with_cache(compilation, &prepared, object, cacheable, options, stdout)
}

fn emit_prepared_object_with_cache(
    compilation: &Compilation,
    prepared: &loom_codegen_llvm::PreparedNativeObject<'_>,
    object: &Path,
    cacheable: bool,
    options: &Options,
    stdout: &mut dyn Write,
) -> io::Result<Result<(), NativePipelineError>> {
    let key = match target_object_key(compilation, prepared, cacheable) {
        Ok(key) => key,
        Err(error) => return Ok(Err(NativePipelineError::Codegen(error))),
    };
    let restored = if let (Some(cache), Some(key)) = (compilation.cache(), key.as_ref()) {
        match cache.load_target_object(key) {
            CacheLookup::Hit(bytes) if cache.materialize(&bytes, object, false).is_ok() => {
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
        if let Err(error) = loom_codegen_llvm::emit_prepared_native_object(prepared, object) {
            return Ok(Err(NativePipelineError::Codegen(error)));
        }
        if let (Some(cache), Some(key), Ok(bytes)) =
            (compilation.cache(), key.as_ref(), std::fs::read(object))
        {
            let _ = cache.store_target_object(key, &bytes);
        }
    }
    Ok(Ok(()))
}

fn restore_cached_artifact(
    compilation: &Compilation,
    key: Option<&CacheKey>,
    output: &Path,
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
    if let Err(error) = cache.materialize(&bytes, output, false) {
        return Ok(Err(error.to_string()));
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
        Some(compilation.sources()),
    )?;
    Ok(compilation.has_errors())
}

fn emit_records(
    records: impl IntoIterator<Item = DiagnosticRecord>,
    json_output: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    sources: Option<&SourceMap>,
) -> io::Result<()> {
    for record in records {
        if json_output {
            write_json_line(
                stdout,
                &serde_json::to_value(record).map_err(io::Error::other)?,
            )?;
        } else {
            writeln!(
                stderr,
                "{}",
                sources.map_or_else(
                    || record.human(),
                    |sources| record.human_with_source(sources)
                )
            )?;
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

fn emit_entry_error(
    options: &Options,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    error: &TargetSelectionError,
) -> io::Result<i32> {
    emit_tool_error(options.json, stdout, stderr, error.code, &error.message)?;
    Ok(EXIT_FAILURE)
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
    let optimization = if take_flag(&mut strings, "--release") {
        loom_codegen_llvm::OptimizationProfile::Release
    } else {
        loom_codegen_llvm::OptimizationProfile::Development
    };
    let target_triple = take_option(&mut strings, "--target-triple")?;
    let runtime_bundle = take_option(&mut strings, "--runtime-bundle")?.map(PathBuf::from);
    let linker = take_option(&mut strings, "--linker")?.map(PathBuf::from);
    let features = parse_feature_list(take_option(&mut strings, "--features")?)?;
    let no_default_features = take_flag(&mut strings, "--no-default-features");
    let locked = take_flag(&mut strings, "--locked");
    let offline = take_flag(&mut strings, "--offline");
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
            offline,
        },
        target_triple,
        runtime_bundle,
        linker,
        optimization,
        program_arguments,
    };
    validate_parsed_options(&options, &strings)?;
    Ok(ParsedArgs::Run(Box::new(options)))
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
        "publish" => Command::Publish {
            registry: take_option(arguments, "--registry")?
                .ok_or_else(|| "publish requires --registry NAME".to_owned())?,
        },
        "runtime" => {
            let operation = arguments
                .first()
                .ok_or_else(|| "runtime requires `pack`".to_owned())?
                .clone();
            arguments.remove(0);
            match operation.as_str() {
                "pack" => Command::RuntimePack {
                    archive: PathBuf::from(
                        take_option(arguments, "--archive")?
                            .ok_or_else(|| "runtime pack requires --archive FILE".to_owned())?,
                    ),
                    output: PathBuf::from(
                        take_option(arguments, "--output")?
                            .ok_or_else(|| "runtime pack requires --output DIR".to_owned())?,
                    ),
                },
                other => return Err(format!("unknown runtime operation `{other}`")),
            }
        }
        "check" => Command::Check,
        "build" => {
            let emit = match take_option(arguments, "--emit")?.as_deref() {
                None | Some("executable") => BuildEmit::Executable,
                Some("object") => BuildEmit::Object,
                Some(other) => {
                    return Err(format!(
                        "unknown build emission `{other}`; expected `executable` or `object`"
                    ));
                }
            };
            let default_output = match (backend, emit) {
                (Backend::Llvm, BuildEmit::Executable) => DEFAULT_NATIVE_ARTIFACT,
                (Backend::Llvm, BuildEmit::Object) => DEFAULT_OBJECT_ARTIFACT,
                (Backend::Interpreter, _) => DEFAULT_INTERPRETED_ARTIFACT,
            };
            Command::Build {
                output: take_option(arguments, "--output")?
                    .map_or_else(|| PathBuf::from(default_output), PathBuf::from),
                entry: take_option(arguments, "--entry")?,
                emit,
            }
        }
        "test" => Command::Test,
        "run" => {
            let entry = take_option(arguments, "--entry")?;
            let artifact = take_option(arguments, "--artifact")?.map(PathBuf::from);
            Command::Run { entry, artifact }
        }
        "debug" => Command::Debug {
            entry: take_option(arguments, "--entry")?,
            debugger: take_option(arguments, "--debugger")?
                .map_or_else(|| PathBuf::from(DEFAULT_DEBUGGER), PathBuf::from),
        },
        "fmt" => Command::Format {
            check: take_flag(arguments, "--check"),
        },
        "cache" => {
            let operation = arguments
                .first()
                .ok_or_else(|| "cache requires `stat` or `prune`".to_owned())?
                .clone();
            arguments.remove(0);
            match operation.as_str() {
                "stat" => Command::Cache { prune: false },
                "prune" => Command::Cache { prune: true },
                other => return Err(format!("unknown cache operation `{other}`")),
            }
        }
        other => return Err(format!("unknown command `{other}`")),
    };
    Ok(command)
}

#[allow(clippy::too_many_lines)]
fn validate_parsed_options(options: &Options, remaining: &[String]) -> Result<(), String> {
    let command = &options.command;
    if !options.program_arguments.is_empty()
        && !matches!(command, Command::Run { .. } | Command::Debug { .. })
    {
        return Err("program arguments after `--` are only valid for run or debug".to_owned());
    }
    if let Some(flag) = remaining.iter().find(|argument| argument.starts_with('-')) {
        return Err(format!("unknown option `{flag}`"));
    }
    if remaining.len() > 1 {
        return Err("expected at most one PATH".to_owned());
    }
    if matches!(command, Command::RuntimePack { .. }) && !remaining.is_empty() {
        return Err("runtime pack does not accept a source PATH".to_owned());
    }
    validate_codegen_options(options)?;
    if options.target.is_some()
        && matches!(
            &command,
            Command::Build { entry: Some(_), .. }
                | Command::Debug { entry: Some(_), .. }
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
        || options.project.lock_mode == LockMode::Locked
        || options.project.offline;
    if resolution_options_used
        && matches!(
            command,
            Command::Format { .. }
                | Command::Publish { .. }
                | Command::RuntimePack { .. }
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
    if options.project.offline && matches!(command, Command::Resolve { refresh: true }) {
        return Err("--offline and resolve --update are mutually exclusive".to_owned());
    }
    if options.target.is_some()
        && matches!(
            &command,
            Command::Resolve { .. }
                | Command::Format { .. }
                | Command::Publish { .. }
                | Command::RuntimePack { .. }
                | Command::Cache { .. }
                | Command::Run {
                    artifact: Some(_),
                    ..
                }
        )
    {
        return Err("--target is only valid for source check/build/test/run/debug".to_owned());
    }
    if (options.no_cache || options.cache_dir.is_some())
        && matches!(
            &command,
            Command::Resolve { .. }
                | Command::Format { .. }
                | Command::Publish { .. }
                | Command::RuntimePack { .. }
                | Command::Run {
                    artifact: Some(_),
                    ..
                }
        )
    {
        return Err(
            "cache options are only valid for source check/build/test/run/debug".to_owned(),
        );
    }
    if options.no_cache && matches!(command, Command::Cache { .. }) {
        return Err("--no-cache is not meaningful for cache stat/prune".to_owned());
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

fn validate_codegen_options(options: &Options) -> Result<(), String> {
    let command = &options.command;
    if options.target_triple.is_some() && !matches!(command, Command::Build { .. }) {
        return Err("--target-triple is only valid for build".to_owned());
    }
    if options.optimization == loom_codegen_llvm::OptimizationProfile::Release
        && matches!(command, Command::Debug { .. })
    {
        return Err(
            "debug always uses the development profile and does not accept --release".to_owned(),
        );
    }
    if options.optimization == loom_codegen_llvm::OptimizationProfile::Release
        && matches!(
            command,
            Command::Resolve { .. }
                | Command::Format { .. }
                | Command::Publish { .. }
                | Command::RuntimePack { .. }
                | Command::Cache { .. }
                | Command::Run {
                    artifact: Some(_),
                    ..
                }
        )
    {
        return Err("--release is only valid for source check/build/test/run".to_owned());
    }
    if options.backend == Backend::Interpreter
        && (matches!(command, Command::Debug { .. })
            || matches!(command, Command::RuntimePack { .. })
            || options.target_triple.is_some()
            || options.optimization == loom_codegen_llvm::OptimizationProfile::Release
            || matches!(
                command,
                Command::Build {
                    emit: BuildEmit::Object,
                    ..
                }
            ))
    {
        return Err(
            "runtime pack, debug, --release, --target-triple, and --emit object require the LLVM backend"
                .to_owned(),
        );
    }
    if options.json && matches!(command, Command::Debug { .. }) {
        return Err("debug is interactive and does not accept --json".to_owned());
    }
    let native_link_options = options.runtime_bundle.is_some() || options.linker.is_some();
    let native_link_command = matches!(
        command,
        Command::Build {
            emit: BuildEmit::Executable,
            ..
        } | Command::Test
            | Command::Run { artifact: None, .. }
            | Command::Debug { .. }
    );
    if native_link_options && !native_link_command {
        return Err(
            "--runtime-bundle and --linker are only valid for native executable build/test/run/debug"
                .to_owned(),
        );
    }
    if native_link_options && options.backend != Backend::Llvm {
        return Err("--runtime-bundle and --linker require the LLVM backend".to_owned());
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

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use loom_driver::{AnalysisHost, PersistentCache, ProjectOptions};

    use super::{
        Backend, Command, Compilation, CompilationData, NativePipelineError, Options,
        adjacent_runtime_bundle_path, configured_runtime_bundle_path, emit_object_with_cache,
    };

    fn valid_sha256(value: &str) -> bool {
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    #[test]
    fn llvm_object_cache_domain_is_pinned() {
        assert_eq!(super::LLVM_OBJECT_CACHE_DOMAIN, "loom-llvm-object-cache-v8");
    }

    #[test]
    fn runtime_bundle_resolution_precedence_is_explicit_environment_then_adjacent() {
        assert_eq!(
            configured_runtime_bundle_path(
                Some(Path::new("explicit")),
                Some(OsStr::new("environment")),
            ),
            Some(PathBuf::from("explicit"))
        );
        assert_eq!(
            configured_runtime_bundle_path(None, Some(OsStr::new("environment"))),
            Some(PathBuf::from("environment"))
        );
        assert_eq!(configured_runtime_bundle_path(None, None), None);
        assert_eq!(
            adjacent_runtime_bundle_path(Path::new("/opt/loom/bin/loomc"))
                .expect("adjacent runtime path"),
            PathBuf::from("/opt/loom/bin/runtime")
        );
    }

    #[test]
    fn checked_mir_cache_identity_pins_interpreted_artifact_version() {
        assert_eq!(loom_mir::INTERPRETED_ARTIFACT_VERSION, 19);
        let context = super::cache_context(loom_mir::LOOM_LANGUAGE_VERSION);
        assert!(
            context
                .frontend_identity
                .ends_with("/loom.interpreted-mir-19"),
            "{}",
            context.frontend_identity
        );
    }

    #[test]
    fn frontend_and_object_build_identities_are_independent_sha256_values() {
        let frontend = env!("LOOM_FRONTEND_BUILD_FINGERPRINT");
        let object = loom_codegen_llvm::LLVM_OBJECT_BUILD_FINGERPRINT;
        assert!(valid_sha256(frontend), "{frontend}");
        assert!(valid_sha256(object), "{object}");
        assert_ne!(frontend, object);
    }

    #[test]
    fn ir_side_artifacts_bypass_the_object_cache_and_are_still_written() {
        let project = tempfile::tempdir().expect("create source project");
        std::fs::write(
            project.path().join("main.loom"),
            "module cache_bypass\n\npub fn main() Unit { Unit }\n",
        )
        .expect("write source fixture");
        let snapshot = AnalysisHost::new(project.path())
            .expect("load source project")
            .snapshot()
            .expect("analyze source project");
        assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
        let cache = tempfile::tempdir().expect("create object cache");
        let compilation = Compilation {
            data: CompilationData::Fresh(Box::new(snapshot)),
            cache: Some(PersistentCache::new(cache.path())),
            key: Some(PersistentCache::semantic_key(
                "cli-cache-bypass-test-v1",
                &[("source", "scalar")],
            )),
        };
        let options = Options {
            command: Command::Check,
            path: project.path().to_path_buf(),
            json: true,
            backend: Backend::Llvm,
            target: None,
            no_cache: false,
            cache_dir: None,
            project: ProjectOptions::default(),
            target_triple: None,
            runtime_bundle: None,
            linker: None,
            optimization: loom_codegen_llvm::OptimizationProfile::Development,
            program_arguments: Vec::new(),
        };
        let ir = project.path().join("program.ll");
        let object = project.path().join("program.o");
        let mut emit_options = loom_codegen_llvm::EmitOptions::run("main");
        emit_options.emit_ir = Some(ir.clone());
        let mut stdout = Vec::new();
        let result: Result<(), NativePipelineError> = emit_object_with_cache(
            &compilation,
            compilation.executable().expect("checked MIR"),
            &object,
            &emit_options,
            &options,
            &mut stdout,
        )
        .expect("run cached object boundary");
        assert!(result.is_ok());
        assert!(object.is_file());
        let llvm = std::fs::read_to_string(ir).expect("read requested LLVM IR");
        assert!(llvm.contains("loom.lcir.fn"), "{llvm}");
        let report = String::from_utf8(stdout).expect("UTF-8 cache report");
        assert!(report.contains("\"layer\":\"target_object\""), "{report}");
        assert!(report.contains("\"status\":\"disabled\""), "{report}");
        assert!(report.contains("\"key\":null"), "{report}");
    }
}
