//! Project discovery and compiler orchestration shared by CLI and LSP.
//!
//! The driver is deliberately the only layer that assigns [`loom_core::FileId`]
//! values, reads projects, applies editor overlays, and orders diagnostics. CLI
//! and LSP consumers therefore observe the same project snapshot.

mod analysis;
mod cache;
mod format;
mod incremental;
mod library;
mod project;
mod registry;
mod report;
mod source;
mod stdlib;
mod symbols;

pub use analysis::{
    AnalysisHost, AnalysisSnapshot, CompilerAdapter, CompilerInput, CompilerOutput,
    ExecutableAdapter, ParseCacheStats, PipelineStage, SemanticQueryStats, StageUnavailable,
};
pub use cache::{
    CACHE_SCHEMA_VERSION, CacheContext, CacheError, CacheKey, CacheLookup, CachePruneReport,
    CacheStats, CachedCompilation, PersistentCache,
};
pub use format::{FormatResult, format_source};
pub use incremental::ModuleInterface;
pub use library::{
    LIBRARY_ARTIFACT_FORMAT, LIBRARY_ARTIFACT_MAX_BYTES, LIBRARY_ARTIFACT_VERSION, LibraryArtifact,
    LibraryArtifactError, decode_library_artifact, encode_library_artifact,
};
pub use project::{
    CURRENT_LANGUAGE_VERSION, LOCK_FILE, LOCK_SCHEMA_VERSION, LockMode, MANIFEST_FILE, Package,
    PackageDependency, PackageId, ProjectGraph, ProjectOptions, Target, TargetKind,
};
pub use registry::{RegistryPublish, publish_registry_package};
pub use report::{DiagnosticRecord, Position, Range, RelatedDiagnostic, SpanRecord};
pub use source::{DriverError, SourceDocument, SourceMap, SourceOrigin, discover_loom_files};
pub use stdlib::identity as stdlib_identity;
pub use symbols::{SymbolId, SymbolInfo, SymbolReference, is_valid_identifier};

/// Successful command completion.
pub const EXIT_SUCCESS: i32 = 0;
/// Source diagnostics, formatter drift, or a failed language test/run.
pub const EXIT_FAILURE: i32 = 1;
/// Invalid invocation/configuration or a compiler stage that is not available.
pub const EXIT_USAGE: i32 = 2;
/// A compiler/interpreter defect at the host boundary.
pub const EXIT_DEFECT: i32 = 3;
