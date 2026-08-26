use std::path::{Path, PathBuf};

use loom_codegen_ir::{
    CheckedArtifact, ReachableSourceGraph, SourceRoots, analyze_source_reachability,
};
use loom_mir::CheckedProgram;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    CodegenError, NATIVE_RUNTIME_ABI, OptimizationProfile,
    emitter::Emitter,
    lcir_emitter::LcirEmitter,
    target::{NativeTargetMachine, create_target_machine},
};

const NATIVE_OBJECT_FORMAT: &str = "loom-legacy-native-object-v5";

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
    /// Stable relative source paths and byte line starts used for DWARF.
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

#[derive(Serialize)]
struct ObjectFingerprint<'a> {
    format: &'static str,
    harness: &'static str,
    backend_version: &'static str,
    backend_build: &'static str,
    llvm_version: (u32, u32, u32),
    runtime_abi: &'static str,
    mir_format: &'static str,
    mir_version: u32,
    target: crate::NativeTargetIdentity,
    target_selection: &'static str,
    roots: &'a SourceRoots,
    reachable: &'a ReachableSourceGraph,
    types: &'a [loom_mir::TypeDef],
    concepts: &'a [loom_mir::ConceptDef],
    requirements: &'a [loom_mir::RequirementDef],
    prelude: &'a loom_mir::PreludeIds,
    functions: Vec<&'a loom_mir::Function>,
    witnesses: Vec<LiveWitness<'a>>,
    debug_sources: &'a [DebugSource],
}

#[derive(Serialize)]
struct LiveWitness<'a> {
    id: loom_mir::WitnessId,
    concept: loom_mir::ConceptId,
    concrete: &'a loom_mir::Type,
    associated: &'a std::collections::BTreeMap<String, loom_mir::Type>,
    type_parameters: u32,
    prerequisites: &'a [loom_mir::WitnessParam],
    methods: Vec<(loom_mir::RequirementId, loom_mir::FunctionId)>,
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
    pub witnesses: usize,
}

/// Relocatable target object emitted from one closed-world root set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeObjectArtifact {
    pub object: PathBuf,
    pub functions: usize,
    pub witnesses: usize,
}

pub(crate) fn select_roots(
    program: &CheckedProgram,
    options: &EmitOptions,
) -> Result<(SourceRoots, ReachableSourceGraph), CodegenError> {
    let roots = match &options.kind {
        EmitKind::Run { entry } => SourceRoots::for_entry(program, entry).ok_or_else(|| {
            CodegenError::new("UnknownEntry", format!("no exported entry named `{entry}`"))
        })?,
        EmitKind::Tests => SourceRoots::for_tests(program),
    };
    let reachable = analyze_source_reachability(program, &roots)?;
    Ok((roots, reachable))
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
    let (roots, reachable) = select_roots(program, options)?;
    let target = create_target_machine(options.target_triple.as_deref(), options.optimization)?;
    legacy_object_fingerprint_with_target(program, options, &roots, &reachable, &target)
}

pub(crate) fn legacy_object_fingerprint_with_target(
    program: &CheckedProgram,
    options: &EmitOptions,
    roots: &SourceRoots,
    reachable: &ReachableSourceGraph,
    target: &NativeTargetMachine,
) -> Result<String, CodegenError> {
    let functions = reachable
        .functions
        .iter()
        .map(|id| {
            program.function(*id).ok_or_else(|| {
                CodegenError::new(
                    "InvalidFunctionReference",
                    format!("reachable function #{} does not exist", id.0),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let witnesses = reachable
        .witnesses
        .iter()
        .map(|id| {
            let witness = program.witness(*id).ok_or_else(|| {
                CodegenError::new(
                    "InvalidWitnessReference",
                    format!("reachable witness #{} does not exist", id.0),
                )
            })?;
            let methods = reachable
                .witness_methods
                .get(id)
                .into_iter()
                .flatten()
                .filter_map(|requirement| {
                    witness
                        .methods
                        .get(requirement)
                        .copied()
                        .map(|function| (*requirement, function))
                })
                .collect();
            Ok(LiveWitness {
                id: witness.id,
                concept: witness.concept,
                concrete: &witness.concrete,
                associated: &witness.associated,
                type_parameters: witness.type_parameters,
                prerequisites: &witness.prerequisites,
                methods,
            })
        })
        .collect::<Result<Vec<_>, CodegenError>>()?;
    let identity = ObjectFingerprint {
        format: NATIVE_OBJECT_FORMAT,
        harness: match options.kind {
            EmitKind::Run { .. } => "run",
            EmitKind::Tests => "tests",
        },
        backend_version: crate::BACKEND_VERSION,
        backend_build: crate::LLVM_OBJECT_BUILD_FINGERPRINT,
        llvm_version: inkwell::support::get_llvm_version(),
        runtime_abi: NATIVE_RUNTIME_ABI,
        mir_format: loom_mir::INTERPRETED_ARTIFACT_FORMAT,
        mir_version: loom_mir::INTERPRETED_ARTIFACT_VERSION,
        target: target.identity(),
        target_selection: target.target_selection(),
        roots,
        reachable,
        types: &program.types,
        concepts: &program.concepts,
        requirements: &program.requirements,
        prelude: &program.prelude,
        functions,
        witnesses,
        debug_sources: &options.debug_sources,
    };
    let bytes = serde_json::to_vec(&identity)
        .map_err(|error| CodegenError::new("ObjectIdentityFailed", error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
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
    let (roots, reachable) = select_roots(program, options)?;
    Emitter::emit_object(program.as_program(), &reachable, &roots, output, options)
}

/// Emits a verified, optimized target object directly from checked typed LCIR.
///
/// This boundary has no checked-MIR fallback and does not infer roots from
/// function names or backend options. The artifact's validated roots select
/// the generated executable harness.
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

/// Links a previously emitted Loom target object with the Rust runtime.
///
/// # Errors
///
/// Returns a stable backend error if the embedded runtime cannot be
/// materialized or the platform linker fails.
pub fn link_native_object(object: &Path, output: &Path) -> Result<(), CodegenError> {
    Emitter::link_object(object, output)
}

/// Rejects linking an object for a non-host triple with the embedded host runtime.
///
/// # Errors
///
/// Returns `CrossLinkUnavailable` when the selected target is not the host.
pub fn validate_native_link_target(options: &EmitOptions) -> Result<(), CodegenError> {
    if crate::target::is_native_target(options.target_triple.as_deref()) {
        Ok(())
    } else {
        Err(CodegenError::new(
            "CrossLinkUnavailable",
            "cross-target executable linking requires a matching Loom runtime and linker; emit an object instead",
        ))
    }
}

/// Emits and links a native executable from checked MIR.
///
/// # Errors
///
/// Returns a stable backend error if root selection, LLVM verification,
/// object emission, or the platform linker fails.
pub fn emit_native(
    program: &CheckedProgram,
    output: &Path,
    options: &EmitOptions,
) -> Result<NativeArtifact, CodegenError> {
    validate_native_link_target(options)?;
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
    let emitted = emit_native_object(program, object.path(), options)?;
    link_native_object(object.path(), output)?;
    if !options.debug_sources.is_empty() {
        crate::emit_native_debug_companion(output)?;
    }
    Ok(NativeArtifact {
        executable: output.to_path_buf(),
        functions: emitted.functions,
        witnesses: emitted.witnesses,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_object_fingerprint_domain_is_pinned() {
        assert_eq!(super::NATIVE_OBJECT_FORMAT, "loom-legacy-native-object-v5");
    }

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
