use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use loom_core::{Diagnostic, FileId, Severity, Span};
use loom_hir::{
    BodyId, BodyKind, Expr as HirExpr, LoweringResult, PackageSourceUnit,
    Statement as HirStatement, lower_package_files,
};
use loom_interpreter::{Interpreter, TestResult};
use loom_lowering::lower_to_mir;
use loom_mir::CheckedProgram as CheckedMirProgram;
use loom_sema::{Analysis, DefMapBuild, ModuleGraph, analyze, analyze_reusing_bodies};
use loom_syntax::{Parse, parse_with_file};
use serde::Serialize;

use crate::incremental::{ModuleQueryKey, module_query_keys};
use crate::source::normalized_project_path;
use crate::{
    CacheLookup, DiagnosticRecord, DriverError, ModuleInterface, PersistentCache, ProjectGraph,
    ProjectOptions, SourceMap,
};

/// Furthest compiler stage completed by a snapshot.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStage {
    Parsed,
    Lowered,
    NamesResolved,
    TypeChecked,
    Executable,
}

/// Explicit host-boundary result when a requested compiler stage is not yet
/// connected. It must not be converted to a successful check/test/run result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StageUnavailable {
    pub code: &'static str,
    pub requested: PipelineStage,
    pub completed: PipelineStage,
    pub message: String,
}

impl fmt::Display for StageUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for StageUnavailable {}

/// Stable input boundary for the semantic/MIR pipeline.
pub struct CompilerInput<'a> {
    pub hir: &'a loom_hir::Program,
    pub analysis: &'a Analysis,
}

/// Output supplied by a semantic/MIR adapter.
#[derive(Clone, Debug)]
pub struct CompilerOutput {
    pub completed: PipelineStage,
    pub diagnostics: Vec<Diagnostic>,
    pub executable: Option<CheckedMirProgram>,
    pub unavailable_reason: Option<String>,
}

/// Adapter kept at the driver boundary while `loom-sema` and MIR lowering
/// stabilize their public end-to-end API.
pub trait CompilerAdapter: Send + Sync {
    fn compile(&self, input: CompilerInput<'_>) -> CompilerOutput;
}

/// Production adapter from error-free typed HIR to validated executable MIR.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExecutableAdapter;

impl CompilerAdapter for ExecutableAdapter {
    fn compile(&self, input: CompilerInput<'_>) -> CompilerOutput {
        if input.analysis.has_errors() {
            return CompilerOutput {
                completed: PipelineStage::TypeChecked,
                diagnostics: Vec::new(),
                executable: None,
                unavailable_reason: Some(
                    "executable lowering requires an error-free program".to_owned(),
                ),
            };
        }
        match lower_to_mir(input.hir, input.analysis) {
            Ok(program) => CompilerOutput {
                completed: PipelineStage::Executable,
                diagnostics: Vec::new(),
                executable: Some(program),
                unavailable_reason: None,
            },
            Err(failure) => CompilerOutput {
                completed: PipelineStage::TypeChecked,
                diagnostics: failure.into_diagnostics(),
                executable: None,
                unavailable_reason: Some("typed HIR to MIR lowering failed".to_owned()),
            },
        }
    }
}

/// Immutable result consumed by both CLI and LSP.
pub struct AnalysisSnapshot {
    project: ProjectGraph,
    sources: SourceMap,
    parses: BTreeMap<FileId, Parse>,
    hir: loom_hir::Program,
    analysis: Analysis,
    diagnostics: Vec<Diagnostic>,
    completed: PipelineStage,
    executable: Option<CheckedMirProgram>,
    unavailable_reason: Option<String>,
    semantic_query_stats: SemanticQueryStats,
}

/// In-process typed-HIR body-query reuse performed by one analysis host.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SemanticQueryStats {
    pub modules_reused: usize,
    pub modules_checked: usize,
    pub bodies_reused: usize,
    pub bodies_checked: usize,
}

impl AnalysisSnapshot {
    #[must_use]
    pub const fn project(&self) -> &ProjectGraph {
        &self.project
    }

    #[must_use]
    pub fn sources(&self) -> &SourceMap {
        &self.sources
    }

    #[must_use]
    pub fn parse(&self, file: FileId) -> Option<&Parse> {
        self.parses.get(&file)
    }

    #[must_use]
    pub const fn hir(&self) -> &loom_hir::Program {
        &self.hir
    }

    #[must_use]
    pub const fn module_graph(&self) -> &ModuleGraph {
        &self.analysis.module_graph
    }

    #[must_use]
    pub const fn def_maps(&self) -> &DefMapBuild {
        &self.analysis.def_maps
    }

    #[must_use]
    pub const fn semantic_analysis(&self) -> &Analysis {
        &self.analysis
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn diagnostic_records(&self) -> Vec<DiagnosticRecord> {
        self.diagnostics
            .iter()
            .filter_map(|diagnostic| DiagnosticRecord::from_diagnostic(diagnostic, &self.sources))
            .collect()
    }

    /// Returns canonical public-interface fingerprints by module name.
    #[must_use]
    pub fn module_interfaces(&self) -> Vec<ModuleInterface> {
        crate::incremental::module_interfaces(&self.sources, &self.parses)
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }

    #[must_use]
    pub const fn completed_stage(&self) -> PipelineStage {
        self.completed
    }

    #[must_use]
    pub const fn semantic_query_stats(&self) -> SemanticQueryStats {
        self.semantic_query_stats
    }

    /// Confirms that the snapshot completed at least `requested`.
    ///
    /// # Errors
    ///
    /// Returns the stable unavailable-stage report when compilation stopped
    /// before the requested boundary.
    pub fn require_stage(&self, requested: PipelineStage) -> Result<(), StageUnavailable> {
        if self.completed >= requested {
            return Ok(());
        }
        Err(StageUnavailable {
            code: "CompilerPipelineIncomplete",
            requested,
            completed: self.completed,
            message: self.unavailable_reason.clone().unwrap_or_else(|| {
                format!(
                    "compiler completed {:?}, but {:?} is required",
                    self.completed, requested
                )
            }),
        })
    }

    /// Returns the validated executable MIR published by the compiler adapter.
    ///
    /// # Errors
    ///
    /// Returns [`StageUnavailable`] if executable lowering did not complete or
    /// an adapter violated its completion contract.
    pub fn executable(&self) -> Result<&CheckedMirProgram, StageUnavailable> {
        self.require_stage(PipelineStage::Executable)?;
        self.executable.as_ref().ok_or_else(|| StageUnavailable {
            code: "CompilerPipelineIncomplete",
            requested: PipelineStage::Executable,
            completed: self.completed,
            message: "compiler adapter reported executable completion without an artifact"
                .to_owned(),
        })
    }

    /// Executes every ordinary language test in deterministic name order.
    ///
    /// # Errors
    ///
    /// Returns [`StageUnavailable`] when this snapshot has no executable MIR.
    pub fn run_tests(&self) -> Result<Vec<TestResult>, StageUnavailable> {
        let program = self.executable()?;
        Ok(Interpreter::new(program).run_tests())
    }
}

/// Mutable owner of editor overlays and the adapter used to produce snapshots.
pub struct AnalysisHost {
    project: ProjectGraph,
    overlays: BTreeMap<PathBuf, String>,
    adapter: Arc<dyn CompilerAdapter>,
    semantic_state: Mutex<Option<SemanticState>>,
}

struct SemanticState {
    keys: BTreeMap<String, ModuleQueryKey>,
    analysis: Analysis,
}

/// Aggregate result of consulting per-source lossless parse entries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ParseCacheStats {
    pub hits: usize,
    pub misses: usize,
}

impl ParseCacheStats {
    #[must_use]
    pub const fn is_full_hit(self) -> bool {
        self.misses == 0 && self.hits != 0
    }
}

impl AnalysisHost {
    /// Opens a project directory or a single `.loom` file.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError`] when the input cannot be canonicalized or read.
    pub fn new(input: impl AsRef<Path>) -> Result<Self, DriverError> {
        Self::with_adapter(input, Arc::new(ExecutableAdapter))
    }

    /// Opens a project with explicit feature and lockfile resolution inputs.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError`] when the project graph cannot be resolved.
    pub fn new_with_options(
        input: impl AsRef<Path>,
        options: &ProjectOptions,
    ) -> Result<Self, DriverError> {
        Self::with_adapter_and_options(input, Arc::new(ExecutableAdapter), options)
    }

    /// Opens a host using an explicit compiler adapter.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError`] when the input cannot be canonicalized or read.
    pub fn with_adapter(
        input: impl AsRef<Path>,
        adapter: Arc<dyn CompilerAdapter>,
    ) -> Result<Self, DriverError> {
        Self::with_adapter_and_options(input, adapter, &ProjectOptions::default())
    }

    fn with_adapter_and_options(
        input: impl AsRef<Path>,
        adapter: Arc<dyn CompilerAdapter>,
        options: &ProjectOptions,
    ) -> Result<Self, DriverError> {
        let project = ProjectGraph::load_with_options(input, options)?;
        Ok(Self {
            project,
            overlays: BTreeMap::new(),
            adapter,
            semantic_state: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        self.project.root()
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectGraph {
        &self.project
    }

    /// Resolves an absolute or project-relative editor path through the same
    /// normalization used by overlays and source lookup.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError`] when the normalized path leaves the project.
    pub fn resolve_path(&self, path: impl AsRef<Path>) -> Result<PathBuf, DriverError> {
        normalized_project_path(self.project.root(), path.as_ref())
    }

    /// Installs or replaces an in-memory source overlay.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError`] when `path` leaves the project.
    pub fn set_overlay(
        &mut self,
        path: impl AsRef<Path>,
        text: impl Into<String>,
    ) -> Result<(), DriverError> {
        let path = self.resolve_path(path)?;
        self.overlays.insert(path, text.into());
        Ok(())
    }

    /// Removes an in-memory source overlay.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError`] when `path` leaves the project.
    pub fn clear_overlay(&mut self, path: impl AsRef<Path>) -> Result<(), DriverError> {
        let path = self.resolve_path(path)?;
        self.overlays.remove(&path);
        Ok(())
    }

    /// Rebuilds an immutable analysis snapshot from disk plus current overlays.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError`] when project discovery or source loading fails.
    pub fn snapshot(&self) -> Result<AnalysisSnapshot, DriverError> {
        let sources = self.load_sources()?;
        Ok(self.snapshot_from_sources(sources))
    }

    /// Loads the exact source set used by the next compiler snapshot.
    ///
    /// This split lets command-line caching hash a stable in-memory source set
    /// and then compile those same bytes, avoiding a hash/compile filesystem
    /// race.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError`] when project discovery or source loading fails.
    pub fn load_sources(&self) -> Result<SourceMap, DriverError> {
        SourceMap::load(&self.project, &self.overlays)
    }

    /// Compiles a source map previously returned by [`Self::load_sources`].
    #[must_use]
    pub fn snapshot_from_sources(&self, sources: SourceMap) -> AnalysisSnapshot {
        let mut diagnostics = Vec::new();
        let mut parses = BTreeMap::new();
        for source in sources.documents() {
            if let Some(text) = source.text() {
                let parse = parse_with_file(source.id(), text);
                diagnostics.extend(parse.diagnostics().iter().cloned());
                validate_package_module(source, &parse, &mut diagnostics);
                parses.insert(source.id(), parse);
            } else {
                let start = source.invalid_utf8_at().unwrap_or(0);
                diagnostics.push(Diagnostic::error(
                    "InvalidUtf8",
                    "source file is not valid UTF-8",
                    Span::new(
                        source.id(),
                        start,
                        start.saturating_add(1).min(source.byte_len()),
                    ),
                ));
            }
        }

        self.finish_snapshot(sources, parses, diagnostics)
    }

    /// Compiles an exact source map while reusing verified per-source parses.
    #[must_use]
    pub fn snapshot_from_sources_with_parse_cache(
        &self,
        sources: SourceMap,
        cache: &PersistentCache,
        compiler_version: &str,
    ) -> (AnalysisSnapshot, ParseCacheStats) {
        let mut diagnostics = Vec::new();
        let mut parses = BTreeMap::new();
        let mut stats = ParseCacheStats::default();
        for source in sources.documents() {
            if let Some(text) = source.text() {
                let key = PersistentCache::source_parse_key(
                    source.relative_path(),
                    text,
                    compiler_version,
                );
                let parse = match cache.load_parse(&key, text, source.id()) {
                    CacheLookup::Hit(parse) => {
                        stats.hits += 1;
                        parse
                    }
                    CacheLookup::Miss => {
                        stats.misses += 1;
                        let parse = parse_with_file(source.id(), text);
                        let _ = cache.store_parse(&key, &parse, text);
                        parse
                    }
                };
                diagnostics.extend(parse.diagnostics().iter().cloned());
                validate_package_module(source, &parse, &mut diagnostics);
                parses.insert(source.id(), parse);
            } else {
                stats.misses += 1;
                let start = source.invalid_utf8_at().unwrap_or(0);
                diagnostics.push(Diagnostic::error(
                    "InvalidUtf8",
                    "source file is not valid UTF-8",
                    Span::new(
                        source.id(),
                        start,
                        start.saturating_add(1).min(source.byte_len()),
                    ),
                ));
            }
        }
        (
            self.finish_snapshot_with_semantic_cache(
                sources,
                parses,
                diagnostics,
                Some((cache, compiler_version)),
            ),
            stats,
        )
    }

    fn finish_snapshot(
        &self,
        sources: SourceMap,
        parses: BTreeMap<FileId, Parse>,
        diagnostics: Vec<Diagnostic>,
    ) -> AnalysisSnapshot {
        self.finish_snapshot_with_semantic_cache(sources, parses, diagnostics, None)
    }

    fn finish_snapshot_with_semantic_cache(
        &self,
        sources: SourceMap,
        parses: BTreeMap<FileId, Parse>,
        mut diagnostics: Vec<Diagnostic>,
        semantic_cache: Option<(&PersistentCache, &str)>,
    ) -> AnalysisSnapshot {
        let LoweringResult {
            program: mut hir,
            diagnostics: lowering_diagnostics,
        } = lower_package_files(parses.iter().map(|(file, parse)| {
            let source = sources
                .document(*file)
                .expect("every parsed file belongs to the source map");
            PackageSourceUnit {
                file: *file,
                package: source.package().cloned().unwrap_or_default(),
                syntax: parse.ast(),
            }
        }));
        self.project.configure_hir_packages(&mut hir);
        diagnostics.extend(lowering_diagnostics);

        let query_keys = module_query_keys(&sources, &parses);
        let source_has_errors = diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error);
        let (analysis, semantic_query_stats) =
            self.analyze_semantics(&hir, query_keys, source_has_errors, semantic_cache);
        diagnostics.extend(analysis.diagnostics.iter().cloned());

        let output = if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            CompilerOutput {
                completed: PipelineStage::TypeChecked,
                diagnostics: Vec::new(),
                executable: None,
                unavailable_reason: Some(
                    "executable lowering requires an error-free source snapshot".to_owned(),
                ),
            }
        } else {
            self.adapter.compile(CompilerInput {
                hir: &hir,
                analysis: &analysis,
            })
        };
        diagnostics.extend(output.diagnostics);
        sort_diagnostics(&mut diagnostics, &sources);

        AnalysisSnapshot {
            project: self.project.clone(),
            sources,
            parses,
            hir,
            analysis,
            diagnostics,
            completed: output.completed,
            executable: output.executable,
            unavailable_reason: output.unavailable_reason,
            semantic_query_stats,
        }
    }

    #[allow(clippy::single_match_else, clippy::too_many_lines)]
    fn analyze_semantics(
        &self,
        hir: &loom_hir::Program,
        keys: BTreeMap<String, ModuleQueryKey>,
        source_has_errors: bool,
        persistent: Option<(&PersistentCache, &str)>,
    ) -> (Analysis, SemanticQueryStats) {
        let mut state = self
            .semantic_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut loaded_persistent_state = false;
        if state.is_none()
            && !source_has_errors
            && let Some((cache, compiler_version)) = persistent
        {
            let cache_key = PersistentCache::typed_module_state_key(&keys, compiler_version);
            if let CacheLookup::Hit(cached) = cache.load_typed_module_state(&cache_key, &keys) {
                *state = Some(SemanticState {
                    keys: cached.keys,
                    analysis: cached.analysis,
                });
                loaded_persistent_state = true;
            }
        }
        let total_bodies = hir.bodies.iter().count();
        let mut reusable_modules = BTreeSet::new();
        let compatible = !source_has_errors
            && state.as_ref().is_some_and(|previous| {
                previous.keys.len() == keys.len()
                    && keys.iter().all(|(module, current)| {
                        previous.keys.get(module).is_some_and(|cached| {
                            cached.interface_fingerprint == current.interface_fingerprint
                                && cached.shape_fingerprint == current.shape_fingerprint
                        })
                    })
            });
        if compatible {
            let previous = state.as_ref().expect("compatible state exists");
            for (module, current) in &keys {
                if previous
                    .keys
                    .get(module)
                    .is_some_and(|cached| cached.body_fingerprint == current.body_fingerprint)
                {
                    reusable_modules.insert(module.clone());
                }
            }
        }
        let reusable_bodies = if compatible {
            hir.bodies
                .iter()
                .filter_map(|(body, definition)| {
                    if loaded_persistent_state
                        && body_may_elide_runtime_validation(&hir.bodies[body])
                    {
                        return None;
                    }
                    let owner = &hir.definitions[definition.owner];
                    let source_module = &hir.modules[owner.module];
                    let module = if source_module.package.name() == "<legacy>" {
                        source_module.name.to_string()
                    } else {
                        format!(
                            "{}@{}+loom{}::{}",
                            source_module.package.name(),
                            source_module.package.version(),
                            source_module.package.language(),
                            source_module.name
                        )
                    };
                    reusable_modules.contains(&module).then_some(body)
                })
                .collect::<BTreeSet<BodyId>>()
        } else {
            BTreeSet::new()
        };
        let mut reuse_survived_validation = compatible;
        let analysis = if compatible {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                analyze_reusing_bodies(
                    hir,
                    &state.as_ref().expect("compatible state exists").analysis,
                    &reusable_bodies,
                )
            })) {
                Ok(analysis) => analysis,
                Err(_) => {
                    reuse_survived_validation = false;
                    analyze(hir)
                }
            }
        } else {
            analyze(hir)
        };
        let (modules_reused, bodies_reused) = if reuse_survived_validation {
            (reusable_modules.len(), reusable_bodies.len())
        } else {
            (0, 0)
        };
        let semantic_stats = SemanticQueryStats {
            modules_reused,
            modules_checked: keys.len().saturating_sub(modules_reused),
            bodies_reused,
            bodies_checked: total_bodies.saturating_sub(bodies_reused),
        };
        if !source_has_errors && !analysis.has_errors() {
            if let Some((cache, compiler_version)) = persistent {
                let cache_key = PersistentCache::typed_module_state_key(&keys, compiler_version);
                let _ = cache.store_typed_module_state(&cache_key, &keys, &analysis);
            }
            *state = Some(SemanticState {
                keys,
                analysis: analysis.clone(),
            });
        }
        (analysis, semantic_stats)
    }
}

fn body_may_elide_runtime_validation(body: &loom_hir::Body) -> bool {
    if !matches!(body.kind, BodyKind::Function | BodyKind::Method) {
        return true;
    }
    body.expressions
        .iter()
        .any(|(_, expression)| match expression {
            HirExpr::Call { .. }
            | HirExpr::MethodCall { .. }
            | HirExpr::QualifiedMethodCall { .. }
            | HirExpr::RecordLiteral { .. } => true,
            HirExpr::Block { statements, .. } => statements
                .iter()
                .any(|statement| matches!(statement, HirStatement::Assert(_))),
            _ => false,
        })
}

fn validate_package_module(
    source: &crate::SourceDocument,
    parse: &Parse,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(declaration) = &parse.ast().module else {
        return;
    };
    let module_name = declaration.name.as_string();
    if crate::standard_library::owns_module(&module_name)
        && source
            .package()
            .is_none_or(|package| package.name() != crate::standard_library::STANDARD_PACKAGE_NAME)
    {
        diagnostics.push(Diagnostic::error(
            "ReservedStandardModule",
            format!("module `{module_name}` is supplied by the compiler-owned standard library"),
            Span {
                file: source.id(),
                range: declaration.range,
            },
        ));
        return;
    }
    let first = declaration
        .name
        .segments
        .first()
        .map(|segment| segment.text.as_str());
    let Some(package) = source.package() else {
        return;
    };
    if first == Some(package.name()) {
        return;
    }
    diagnostics.push(Diagnostic::error(
        "PackageModuleNamespace",
        format!(
            "module `{}` must be inside package namespace `{}`",
            declaration.name.as_string(),
            package.name()
        ),
        Span {
            file: source.id(),
            range: declaration.range,
        },
    ));
}

fn sort_diagnostics(diagnostics: &mut [Diagnostic], sources: &SourceMap) {
    diagnostics
        .sort_by(|left, right| diagnostic_key(left, sources).cmp(&diagnostic_key(right, sources)));
    for diagnostic in diagnostics {
        diagnostic.labels.sort_by(|left, right| {
            span_sort_key(left.span, sources)
                .cmp(&span_sort_key(right.span, sources))
                .then(left.message.cmp(&right.message))
        });
    }
}

fn diagnostic_key<'a>(
    diagnostic: &'a Diagnostic,
    sources: &'a SourceMap,
) -> (&'a str, u32, u32, &'a str, &'a str) {
    let (path, start, end) = span_sort_key(diagnostic.primary, sources);
    (path, start, end, &diagnostic.code, &diagnostic.message)
}

fn span_sort_key(span: Span, sources: &SourceMap) -> (&str, u32, u32) {
    let path = sources
        .document(span.file)
        .map_or("", crate::SourceDocument::relative_path);
    (path, span.range.start, span.range.end)
}
