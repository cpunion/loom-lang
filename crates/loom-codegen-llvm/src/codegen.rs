use std::path::{Path, PathBuf};

use loom_mir::Program;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{CodegenError, OptimizationProfile, ReachableProgram, Roots, emitter::Emitter};

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
    backend_version: &'static str,
    mir_format: &'static str,
    mir_version: u32,
    target: crate::NativeTargetIdentity,
    roots: &'a Roots,
    reachable: &'a ReachableProgram,
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

fn select_roots(
    program: &Program,
    options: &EmitOptions,
) -> Result<(Roots, ReachableProgram), CodegenError> {
    let roots = match &options.kind {
        EmitKind::Run { entry } => Roots::for_entry(program, entry).ok_or_else(|| {
            CodegenError::new("UnknownEntry", format!("no exported entry named `{entry}`"))
        })?,
        EmitKind::Tests => Roots::for_tests(program),
    };
    let reachable = crate::analyze_reachability(program, &roots)?;
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
    program: &Program,
    options: &EmitOptions,
) -> Result<String, CodegenError> {
    let (roots, reachable) = select_roots(program, options)?;
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
        format: "loom-native-object-v2",
        backend_version: crate::BACKEND_VERSION,
        mir_format: loom_mir::INTERPRETED_ARTIFACT_FORMAT,
        mir_version: loom_mir::INTERPRETED_ARTIFACT_VERSION,
        target: crate::target_identity(options.target_triple.as_deref(), options.optimization)?,
        roots: &roots,
        reachable: &reachable,
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
    program: &Program,
    output: &Path,
    options: &EmitOptions,
) -> Result<NativeObjectArtifact, CodegenError> {
    let (roots, reachable) = select_roots(program, options)?;
    Emitter::emit_object(program, &reachable, &roots, output, options)
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
    program: &Program,
    output: &Path,
    options: &EmitOptions,
) -> Result<NativeArtifact, CodegenError> {
    validate_native_link_target(options)?;
    let object = tempfile::Builder::new()
        .prefix("loom-")
        .suffix(".o")
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
