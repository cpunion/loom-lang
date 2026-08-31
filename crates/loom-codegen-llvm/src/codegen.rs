use std::path::{Path, PathBuf};

use loom_codegen_ir::CheckedArtifact;
use loom_mir::CheckedProgram;
use serde::Serialize;

use crate::{
    CodegenError, OptimizationProfile,
    lcir_emitter::LcirEmitter,
    prepared::{
        emit_prepared_native_object, prepare_native_object, prepared_native_object_fingerprint,
        prepared_native_target_identity,
    },
};

/// Native executable harness selected by the CLI command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EmitKind {
    Run { entry: String },
    Tests,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmitOptions {
    pub kind: EmitKind,
    /// Optional LLVM IR side artifact, useful for diagnostics and golden tests.
    pub emit_ir: Option<PathBuf>,
    /// Stable relative source paths and byte line starts used for native debug
    /// metadata (DWARF or `CodeView` according to the target).
    pub debug_sources: Vec<DebugSource>,
    /// Explicit normalized LLVM target triple, or the host target when absent.
    pub target_triple: Option<String>,
    pub optimization: OptimizationProfile,
}

/// Target and side-artifact policy for emitting one already-rooted LCIR artifact.
///
/// Roots deliberately do not appear here: [`CheckedArtifact`] is the sole
/// authority for run-versus-tests selection and its closed callable graph.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeObjectOptions {
    /// Optional LLVM IR side artifact, useful for diagnostics and golden tests.
    pub emit_ir: Option<PathBuf>,
    /// Stable source inputs used for LCIR compile-unit, function, and location
    /// metadata. LCIR signatures describe the physical compiler ABI: fallible
    /// results use `LoomFallible<T>`, and the hidden fault-context parameter is
    /// present but marked artificial.
    pub debug_sources: Vec<DebugSource>,
    /// Explicit normalized LLVM target triple, or the host target when absent.
    pub target_triple: Option<String>,
    pub optimization: OptimizationProfile,
}

impl NativeObjectOptions {
    #[must_use]
    pub fn with_debug_sources(mut self, sources: Vec<DebugSource>) -> Self {
        self.debug_sources = sources;
        self
    }

    #[must_use]
    pub fn with_target_triple(mut self, triple: Option<String>) -> Self {
        self.target_triple = triple;
        self
    }

    #[must_use]
    pub const fn with_optimization(mut self, optimization: OptimizationProfile) -> Self {
        self.optimization = optimization;
        self
    }
}

/// Relocation-independent source metadata consumed only by native debug info.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DebugSource {
    pub file: u32,
    pub path: String,
    pub line_starts: Vec<u32>,
}

impl DebugSource {
    #[must_use]
    pub fn new(file: u32, path: impl Into<String>, text: &str) -> Self {
        let mut line_starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n'
                && let Ok(next) = u32::try_from(offset.saturating_add(1))
            {
                line_starts.push(next);
            }
        }
        Self {
            file,
            path: path.into(),
            line_starts,
        }
    }
}

impl EmitOptions {
    #[must_use]
    pub fn run(entry: impl Into<String>) -> Self {
        Self {
            kind: EmitKind::Run {
                entry: entry.into(),
            },
            emit_ir: None,
            debug_sources: Vec::new(),
            target_triple: None,
            optimization: OptimizationProfile::Development,
        }
    }

    #[must_use]
    pub const fn tests() -> Self {
        Self {
            kind: EmitKind::Tests,
            emit_ir: None,
            debug_sources: Vec::new(),
            target_triple: None,
            optimization: OptimizationProfile::Development,
        }
    }

    #[must_use]
    pub fn with_debug_sources(mut self, sources: Vec<DebugSource>) -> Self {
        self.debug_sources = sources;
        self
    }

    #[must_use]
    pub fn with_target_triple(mut self, triple: Option<String>) -> Self {
        self.target_triple = triple;
        self
    }

    #[must_use]
    pub const fn with_optimization(mut self, optimization: OptimizationProfile) -> Self {
        self.optimization = optimization;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeArtifact {
    pub executable: PathBuf,
    pub functions: usize,
}

/// Relocatable target object emitted from one closed-world root set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeObjectArtifact {
    pub object: PathBuf,
    pub functions: usize,
}

/// Computes the closed-world semantic identity of the target object.
///
/// Unreachable function bodies and unused witness method slots are deliberately
/// absent, so a private dead body edit can reuse an existing optimized object.
///
/// # Errors
///
/// Returns a stable backend error when roots/reachability are invalid, the
/// native target is unavailable, or the canonical identity cannot be encoded.
pub fn native_object_fingerprint(
    program: &CheckedProgram,
    options: &EmitOptions,
) -> Result<String, CodegenError> {
    let prepared = prepare_native_object(program, options.clone()).map_err(CodegenError::from)?;
    prepared_native_object_fingerprint(&prepared)
}

/// Emits a verified, optimized target object without invoking the native linker.
///
/// The object path is caller-selected so a driver can put this boundary behind
/// a content-addressed cache and link the same bytes into multiple outputs.
///
/// # Errors
///
/// Returns a stable backend error if root selection, reachability, LLVM
/// verification, optimization, or object emission fails.
pub fn emit_native_object(
    program: &CheckedProgram,
    output: &Path,
    options: &EmitOptions,
) -> Result<NativeObjectArtifact, CodegenError> {
    let prepared = prepare_native_object(program, options.clone()).map_err(CodegenError::from)?;
    emit_prepared_native_object(&prepared, output)
}

/// Emits a verified, optimized target object directly from checked typed LCIR.
///
/// This boundary does not infer roots from function names or backend options.
/// The artifact's validated roots select the generated executable harness.
///
/// # Errors
///
/// Returns a stable backend error if the LCIR target layout disagrees with the
/// selected LLVM target, or LLVM verification, optimization, IR writing, or
/// object emission fails.
pub fn emit_lcir_native_object(
    artifact: &CheckedArtifact,
    output: &Path,
    options: &NativeObjectOptions,
) -> Result<NativeObjectArtifact, CodegenError> {
    LcirEmitter::emit_object(artifact, output, options)
}

/// Emits and links a native executable through typed LCIR.
///
/// # Errors
///
/// Returns a stable backend error if root selection, LLVM verification, object
/// emission, runtime-bundle validation, or the platform linker fails.
pub fn emit_native(
    program: &CheckedProgram,
    output: &Path,
    options: &EmitOptions,
    runtime: &crate::RuntimeBundle,
    linker: &crate::RuntimeLinker,
) -> Result<NativeArtifact, CodegenError> {
    let prepared = prepare_native_object(program, options.clone()).map_err(CodegenError::from)?;
    let expected = prepared_native_target_identity(&prepared);
    if runtime.target_triple() != expected.triple || runtime.data_layout() != expected.data_layout {
        return Err(CodegenError::new(
            "RuntimeBundleTargetMismatch",
            "runtime bundle target triple/data layout does not match the emitted object",
        ));
    }
    let object_extension = crate::native_artifact_extension(
        options.target_triple.as_deref(),
        crate::NativeArtifactKind::Object,
    )
    .unwrap_or("o");
    let object_suffix = format!(".{object_extension}");
    let object = tempfile::Builder::new()
        .prefix("loom-")
        .suffix(&object_suffix)
        .tempfile()
        .map_err(|error| CodegenError::new("ArtifactWriteFailed", error.to_string()))?;
    let emitted = emit_prepared_native_object(&prepared, object.path())?;
    // MSVC linkers must reopen the object and Windows temporary files do not
    // grant that access while NamedTempFile still owns its creation handle.
    // Keep deletion ownership while closing the handle before link execution.
    let object = object.into_temp_path();
    crate::link_object_with_runtime_bundle(&object, output, runtime, linker)?;
    if !options.debug_sources.is_empty() {
        crate::emit_native_debug_companion(output)?;
    }
    Ok(NativeArtifact {
        executable: output.to_path_buf(),
        functions: emitted.functions,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn object_build_identity_and_loaded_llvm_are_pinned() {
        let build = crate::LLVM_OBJECT_BUILD_FINGERPRINT;
        assert_eq!(build.len(), 64, "{build}");
        assert!(
            build.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{build}"
        );
        let version = inkwell::support::get_llvm_version();
        assert_eq!(version.0, 19, "loaded LLVM version is {version:?}");
    }
}
