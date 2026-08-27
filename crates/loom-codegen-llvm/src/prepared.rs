//! One-shot native route preparation shared by cache identity and emission.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{self, Write};
use std::path::Path;

use loom_codegen_ir::{
    CheckedArtifact, InvalidRootCode, LoweringDefectCode, LoweringError, LoweringOutcome,
    ReachableSourceGraph, ResourceLimitCode, SourceArtifactRequest, SourceRoots, TargetLayout,
    analyze_source_reachability, lower_typed_artifact, write_artifact_identity,
};
use loom_mir::{CheckedProgram, Type};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::codegen::{
    DebugSource, EmitKind, EmitOptions, NativeObjectArtifact, NativeObjectOptions,
    legacy_object_fingerprint_with_target,
};
use crate::emitter::Emitter;
use crate::lcir_emitter::LcirEmitter;
use crate::target::{NATIVE_RUNTIME_ABI, NativeTargetMachine, create_llvm_target_machine};
use crate::{CodegenError, NativeTargetIdentity, trace_llvm_stage};

const LCIR_NATIVE_OBJECT_FORMAT: &str = "loom-lcir-native-object-v8";

/// Policy controlling the whole-artifact native route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeRoutePolicy {
    /// Prefer typed LCIR and use checked-MIR code generation only when the
    /// complete reachable artifact is outside current LCIR coverage.
    Automatic,
    /// Use the checked-MIR backend without attempting LCIR lowering.
    LegacyOnly,
}

/// The immutable route selected for one prepared native object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeRouteKind {
    Lcir,
    Legacy,
}

/// Stable class for a failure before a native route can be prepared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativePreparationErrorKind {
    InvalidRoot,
    Resource,
    Target,
    Defect,
}

/// Structured preparation failure. Unsupported LCIR is deliberately absent:
/// it is the sole successful transition to the legacy route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativePreparationError {
    kind: NativePreparationErrorKind,
    code: &'static str,
    message: String,
}

impl NativePreparationError {
    #[must_use]
    pub const fn kind(&self) -> NativePreparationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(
        kind: NativePreparationErrorKind,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
        }
    }

    fn target(error: &CodegenError) -> Self {
        Self::new(
            NativePreparationErrorKind::Target,
            error.code(),
            error.message(),
        )
    }

    fn lowering(error: LoweringError) -> Self {
        match error {
            LoweringError::InvalidRoot { code, message } => Self::new(
                NativePreparationErrorKind::InvalidRoot,
                invalid_root_error_code(code),
                message,
            ),
            LoweringError::ResourceLimit { code, message } => Self::new(
                NativePreparationErrorKind::Resource,
                resource_error_code(code),
                message,
            ),
            LoweringError::Defect { code, message } => Self::new(
                NativePreparationErrorKind::Defect,
                lowering_defect_error_code(code),
                message,
            ),
        }
    }

    fn invalid_root(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(NativePreparationErrorKind::InvalidRoot, code, message)
    }

    fn defect(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(NativePreparationErrorKind::Defect, code, message)
    }
}

impl fmt::Display for NativePreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for NativePreparationError {}

const fn invalid_root_error_code(code: InvalidRootCode) -> &'static str {
    match code {
        InvalidRootCode::UnknownEntry => "NativePreparationUnknownEntry",
        InvalidRootCode::InvalidFunction => "NativePreparationInvalidRootFunction",
        InvalidRootCode::DuplicateTest => "NativePreparationDuplicateTestRoot",
        InvalidRootCode::RootSignature => "NativePreparationRootSignature",
    }
}

const fn resource_error_code(code: ResourceLimitCode) -> &'static str {
    match code {
        ResourceLimitCode::ProgramTooLarge => "NativePreparationProgramTooLarge",
    }
}

const fn lowering_defect_error_code(code: LoweringDefectCode) -> &'static str {
    match code {
        LoweringDefectCode::SourceGraph => "NativePreparationSourceGraphDefect",
        LoweringDefectCode::InconsistentPlan => "NativePreparationInconsistentPlan",
        LoweringDefectCode::Builder => "NativePreparationBuilderDefect",
        LoweringDefectCode::GeneratedProgram => "NativePreparationGeneratedProgramDefect",
        LoweringDefectCode::GeneratedArtifact => "NativePreparationGeneratedArtifactDefect",
    }
}

enum PreparedRoute<'mir> {
    Lcir(CheckedArtifact),
    Legacy {
        mir: &'mir CheckedProgram,
        roots: SourceRoots,
        reachable: ReachableSourceGraph,
    },
}

/// Opaque native object plan prepared exactly once and consumed on the same
/// thread for fingerprinting and emission.
pub struct PreparedNativeObject<'mir> {
    target: NativeTargetMachine,
    target_identity: NativeTargetIdentity,
    options: EmitOptions,
    route: PreparedRoute<'mir>,
}

impl PreparedNativeObject<'_> {
    #[must_use]
    pub const fn route_kind(&self) -> NativeRouteKind {
        match self.route {
            PreparedRoute::Lcir(_) => NativeRouteKind::Lcir,
            PreparedRoute::Legacy { .. } => NativeRouteKind::Legacy,
        }
    }
}

/// Prepares one immutable whole-artifact route and its exact LLVM target.
///
/// # Errors
///
/// Returns a structured invalid-root, resource, target, or compiler-defect
/// error. Valid but unsupported LCIR always succeeds as one whole legacy plan.
pub fn prepare_native_object(
    mir: &CheckedProgram,
    options: EmitOptions,
    policy: NativeRoutePolicy,
) -> Result<PreparedNativeObject<'_>, NativePreparationError> {
    trace_llvm_stage("prepare.target.begin");
    let target = create_llvm_target_machine(options.target_triple.as_deref(), options.optimization)
        .map_err(|error| NativePreparationError::target(&error))?;
    trace_llvm_stage("prepare.target.end");
    trace_llvm_stage("prepare.identity.begin");
    let target_identity = target.identity();
    trace_llvm_stage("prepare.identity.end");
    let route = match policy {
        NativeRoutePolicy::Automatic => {
            let llvm_pointer_bits = target
                .pointer_bits()
                .map_err(|error| NativePreparationError::target(&error))?;
            let pointer_bits = u16::try_from(llvm_pointer_bits).map_err(|_| {
                NativePreparationError::new(
                    NativePreparationErrorKind::Target,
                    "LcirTargetLayoutUnavailable",
                    format!(
                        "LLVM target {} pointer width does not fit LCIR target layout",
                        target.triple
                    ),
                )
            })?;
            let layout = TargetLayout::new(pointer_bits).map_err(|error| {
                NativePreparationError::new(
                    NativePreparationErrorKind::Target,
                    "LcirTargetLayoutUnavailable",
                    error.to_string(),
                )
            })?;
            let request = source_request(&options);
            trace_llvm_stage("prepare.lcir-lowering.begin");
            match lower_typed_artifact(mir, &request, layout)
                .map_err(NativePreparationError::lowering)?
            {
                LoweringOutcome::Complete(artifact) => {
                    trace_llvm_stage("prepare.lcir-lowering.complete");
                    PreparedRoute::Lcir(artifact)
                }
                LoweringOutcome::Unsupported(_) => {
                    trace_llvm_stage("prepare.lcir-lowering.unsupported");
                    target
                        .validate_legacy_value_abi()
                        .map_err(|error| NativePreparationError::target(&error))?;
                    let (roots, reachable) = legacy_graph_after_validated_lowering(mir, &options)?;
                    PreparedRoute::Legacy {
                        mir,
                        roots,
                        reachable,
                    }
                }
            }
        }
        NativeRoutePolicy::LegacyOnly => {
            let roots = validated_legacy_roots(mir, &options)?;
            let reachable = analyze_source_reachability(mir, &roots).map_err(|error| {
                NativePreparationError::defect(
                    "NativePreparationSourceGraphDefect",
                    format!("checked-MIR reachability failed: {error}"),
                )
            })?;
            target
                .validate_legacy_value_abi()
                .map_err(|error| NativePreparationError::target(&error))?;
            PreparedRoute::Legacy {
                mir,
                roots,
                reachable,
            }
        }
    };
    Ok(PreparedNativeObject {
        target,
        target_identity,
        options,
        route,
    })
}

/// Returns the exact target identity owned by a prepared object plan.
#[must_use]
pub const fn prepared_native_target_identity<'prepared>(
    prepared: &'prepared PreparedNativeObject<'_>,
) -> &'prepared NativeTargetIdentity {
    &prepared.target_identity
}

/// Computes the route-separated semantic identity without repeating lowering,
/// root selection, reachability, or target-machine creation.
///
/// # Errors
///
/// Returns an object-identity error if canonical encoding fails.
pub fn prepared_native_object_fingerprint(
    prepared: &PreparedNativeObject<'_>,
) -> Result<String, CodegenError> {
    trace_llvm_stage("prepare.fingerprint.begin");
    let fingerprint = match &prepared.route {
        PreparedRoute::Lcir(artifact) => lcir_object_fingerprint(prepared, artifact),
        PreparedRoute::Legacy {
            mir,
            roots,
            reachable,
        } => legacy_object_fingerprint_with_target(
            mir,
            &prepared.options,
            roots,
            reachable,
            &prepared.target,
        ),
    }?;
    trace_llvm_stage("prepare.fingerprint.end");
    Ok(fingerprint)
}

/// Emits the route selected during preparation using the same target machine.
///
/// # Errors
///
/// Returns the selected emitter's error directly. Emission failures never
/// trigger a route change or fallback.
pub fn emit_prepared_native_object(
    prepared: &PreparedNativeObject<'_>,
    output: &Path,
) -> Result<NativeObjectArtifact, CodegenError> {
    trace_llvm_stage("prepare.emission.begin");
    let artifact = match &prepared.route {
        PreparedRoute::Lcir(artifact) => LcirEmitter::emit_object_with_target(
            artifact,
            output,
            &lcir_options(&prepared.options),
            &prepared.target,
        ),
        PreparedRoute::Legacy {
            mir,
            roots,
            reachable,
        } => Emitter::emit_object_with_target(
            mir.as_program(),
            reachable,
            roots,
            output,
            &prepared.options,
            &prepared.target,
        ),
    }?;
    trace_llvm_stage("prepare.emission.end");
    Ok(artifact)
}

fn source_request(options: &EmitOptions) -> SourceArtifactRequest {
    match &options.kind {
        EmitKind::Run { entry } => SourceArtifactRequest::Run {
            entry: entry.clone(),
        },
        EmitKind::Tests => SourceArtifactRequest::Tests,
    }
}

fn legacy_graph_after_validated_lowering(
    mir: &CheckedProgram,
    options: &EmitOptions,
) -> Result<(SourceRoots, ReachableSourceGraph), NativePreparationError> {
    let roots = raw_roots(mir, options).ok_or_else(|| {
        NativePreparationError::defect(
            "NativePreparationInconsistentPlan",
            "LCIR classification accepted a root which disappeared before legacy preparation",
        )
    })?;
    let reachable = analyze_source_reachability(mir, &roots).map_err(|error| {
        NativePreparationError::defect(
            "NativePreparationSourceGraphDefect",
            format!("checked-MIR reachability failed after LCIR classification: {error}"),
        )
    })?;
    Ok((roots, reachable))
}

fn validated_legacy_roots(
    mir: &CheckedProgram,
    options: &EmitOptions,
) -> Result<SourceRoots, NativePreparationError> {
    let roots = match &options.kind {
        EmitKind::Run { entry } => SourceRoots::for_entry(mir, entry).ok_or_else(|| {
            NativePreparationError::invalid_root(
                "NativePreparationUnknownEntry",
                format!("run entry `{entry}` is not exported"),
            )
        })?,
        EmitKind::Tests => {
            let mut seen = BTreeSet::new();
            for (index, root) in mir.tests.iter().copied().enumerate() {
                if !seen.insert(root) {
                    return Err(NativePreparationError::invalid_root(
                        "NativePreparationDuplicateTestRoot",
                        format!("test root #{} at index {index} is duplicated", root.0),
                    ));
                }
            }
            SourceRoots::for_tests(mir)
        }
    };
    validate_root_signatures(mir, options, &roots)?;
    Ok(roots)
}

fn raw_roots(mir: &CheckedProgram, options: &EmitOptions) -> Option<SourceRoots> {
    match &options.kind {
        EmitKind::Run { entry } => SourceRoots::for_entry(mir, entry),
        EmitKind::Tests => Some(SourceRoots::for_tests(mir)),
    }
}

fn validate_root_signatures(
    mir: &CheckedProgram,
    options: &EmitOptions,
    roots: &SourceRoots,
) -> Result<(), NativePreparationError> {
    let tests = matches!(options.kind, EmitKind::Tests);
    for root in roots.functions() {
        let function = mir.function(*root).ok_or_else(|| {
            NativePreparationError::invalid_root(
                "NativePreparationInvalidRootFunction",
                format!("artifact root function #{} does not exist", root.0),
            )
        })?;
        let hidden_inputs = function.type_parameters != 0
            || !function.witness_params.is_empty()
            || function.witness_prefix_count != 0
            || function.receiver.is_some();
        let invalid = if tests {
            hidden_inputs
                || !function.params.is_empty()
                || !is_valid_test_return(mir, &function.return_ty)
        } else {
            hidden_inputs || !function.params.is_empty() || function.return_ty != Type::Unit
        };
        if invalid {
            let expected = if tests {
                "have no inputs and return Unit or Result[Unit, E]"
            } else {
                "have signature () -> Unit"
            };
            return Err(NativePreparationError::invalid_root(
                "NativePreparationRootSignature",
                format!("artifact root `{}` must {expected}", function.name),
            ));
        }
    }
    Ok(())
}

fn is_valid_test_return(program: &CheckedProgram, ty: &Type) -> bool {
    if *ty == Type::Unit {
        return true;
    }
    let Some(result) = program.prelude.result else {
        return false;
    };
    matches!(
        ty,
        Type::Nominal(type_id, arguments)
            if *type_id == result
                && arguments.len() == 2
                && arguments.first() == Some(&Type::Unit)
    )
}

fn lcir_options(options: &EmitOptions) -> NativeObjectOptions {
    NativeObjectOptions {
        emit_ir: options.emit_ir.clone(),
        debug_sources: options.debug_sources.clone(),
        target_triple: options.target_triple.clone(),
        optimization: options.optimization,
    }
}

#[derive(Serialize)]
struct LcirFingerprintHeader<'a> {
    format: &'static str,
    backend_version: &'static str,
    backend_build: &'static str,
    llvm_version: (u32, u32, u32),
    runtime_abi: &'static str,
    target: &'a NativeTargetIdentity,
    target_selection: &'static str,
    debug_sources: &'a [DebugSource],
}

fn lcir_object_fingerprint(
    prepared: &PreparedNativeObject<'_>,
    artifact: &CheckedArtifact,
) -> Result<String, CodegenError> {
    let header = LcirFingerprintHeader {
        format: LCIR_NATIVE_OBJECT_FORMAT,
        backend_version: crate::BACKEND_VERSION,
        backend_build: crate::LLVM_OBJECT_BUILD_FINGERPRINT,
        llvm_version: inkwell::support::get_llvm_version(),
        runtime_abi: NATIVE_RUNTIME_ABI,
        target: &prepared.target_identity,
        target_selection: prepared.target.target_selection(),
        debug_sources: &prepared.options.debug_sources,
    };
    let header = serde_json::to_vec(&header)
        .map_err(|error| CodegenError::new("ObjectIdentityFailed", error.to_string()))?;
    let mut digest = Sha256::new();
    digest.update(
        u64::try_from(header.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    digest.update(&header);
    write_artifact_identity(artifact, &mut DigestFormatter(&mut digest))
        .map_err(|_| CodegenError::new("ObjectIdentityFailed", "cannot encode LCIR identity"))?;
    Ok(format!("{:x}", digest.finalize()))
}

struct DigestFormatter<'a>(&'a mut Sha256);

impl Write for DigestFormatter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.0.update(value.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lcir_object_fingerprint_domain_is_pinned() {
        assert_eq!(
            super::LCIR_NATIVE_OBJECT_FORMAT,
            "loom-lcir-native-object-v8"
        );
    }
}
