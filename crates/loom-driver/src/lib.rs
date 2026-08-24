//! Project discovery and compiler orchestration shared by CLI and LSP.
//!
//! The driver is deliberately the only layer that assigns [`loom_core::FileId`]
//! values, reads projects, applies editor overlays, and orders diagnostics. CLI
//! and LSP consumers therefore observe the same project snapshot.

mod analysis;
mod cache;
mod format;
mod incremental;
mod project;
mod report;
mod source;
mod symbols;

pub use analysis::{
    AnalysisHost, AnalysisSnapshot, CompilerAdapter, CompilerInput, CompilerOutput,
    ExecutableAdapter, ParseCacheStats, PipelineStage, SemanticQueryStats, StageUnavailable,
};
pub use cache::{
    CacheContext, CacheError, CacheKey, CacheLookup, CachedCompilation, PersistentCache,
};
pub use format::{FormatResult, format_source};
pub use incremental::ModuleInterface;
pub use project::{
    MANIFEST_FILE, Package, PackageDependency, PackageId, ProjectGraph, Target, TargetKind,
};
pub use report::{DiagnosticRecord, Position, Range, RelatedDiagnostic, SpanRecord};
pub use source::{DriverError, SourceDocument, SourceMap, discover_loom_files};
pub use symbols::{SymbolInfo, SymbolReference, is_valid_identifier};

/// Successful command completion.
pub const EXIT_SUCCESS: i32 = 0;
/// Source diagnostics, formatter drift, or a failed language test/run.
pub const EXIT_FAILURE: i32 = 1;
/// Invalid invocation/configuration or a compiler stage that is not available.
pub const EXIT_USAGE: i32 = 2;
/// A compiler/interpreter defect at the host boundary.
pub const EXIT_DEFECT: i32 = 3;
