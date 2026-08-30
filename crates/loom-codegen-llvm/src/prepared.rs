//! One-shot native route preparation shared by cache identity and emission.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{self, Write};
use std::path::Path;

use loom_codegen_ir::{
    CheckedArtifact, InvalidRootCode, LoweringDefectCode, LoweringError, LoweringOutcome,
    ReachableSourceGraph, ResourceLimitCode, SourceArtifactRequest, SourceRoots, SupportReport,
    TargetLayout, analyze_source_reachability, lower_typed_artifact, write_artifact_identity,
};
use loom_mir::CheckedProgram;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::codegen::{
    DebugSource, EmitKind, EmitOptions, NativeObjectArtifact, NativeObjectOptions,
    checked_mir_object_fingerprint_with_target, validate_checked_mir_root_signatures,
};
use crate::emitter::Emitter;
use crate::lcir_emitter::LcirEmitter;
use crate::target::{NATIVE_RUNTIME_ABI, NativeTargetMachine, create_llvm_target_machine};
use crate::{CodegenError, NativeTargetIdentity, trace_llvm_stage};

const LCIR_NATIVE_OBJECT_FORMAT: &str = "loom-lcir-native-object-v41";

/// Policy controlling the whole-artifact native route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeRoutePolicy {
    /// Prefer typed LCIR and use checked-MIR code generation only when the
    /// complete reachable artifact is outside current LCIR coverage.
    Automatic,
    /// Require complete typed LCIR coverage and reject the complete artifact
    /// with its deterministic support report instead of selecting checked-MIR
    /// code generation.
    LcirOnly,
    /// Use the checked-MIR backend without attempting LCIR lowering.
    CheckedMirOnly,
}

/// The immutable route selected for one prepared native object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeRouteKind {
    Lcir,
    CheckedMir,
}

/// Stable class for a failure before a native route can be prepared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativePreparationErrorKind {
    InvalidRoot,
    Unsupported,
    Resource,
    Target,
    Defect,
}

/// Structured preparation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativePreparationError {
    kind: NativePreparationErrorKind,
    code: &'static str,
    message: String,
    support_report: Option<SupportReport>,
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

    /// Returns the deterministic whole-artifact LCIR coverage report for an
    /// [`NativePreparationErrorKind::Unsupported`] failure.
    #[must_use]
    pub const fn support_report(&self) -> Option<&SupportReport> {
        self.support_report.as_ref()
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
            support_report: None,
        }
    }

    fn unsupported(report: SupportReport) -> Self {
        let message = unsupported_message(&report);
        Self {
            kind: NativePreparationErrorKind::Unsupported,
            code: "NativePreparationUnsupportedLcir",
            message,
            support_report: Some(report),
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

fn unsupported_message(report: &SupportReport) -> String {
    let mut message = format!(
        "typed LCIR is required, but reachable MIR uses {} unsupported LCIR feature site(s)",
        report.len()
    );
    for item in report.items() {
        let expression = item.expression().map_or_else(
            || "none".to_owned(),
            |expression| format!("#{}", expression.0),
        );
        let span = item.span();
        write!(
            message,
            "\n- {}: {} (function #{}, expression {expression}, file #{}, bytes {}..{})",
            item.feature(),
            item.path(),
            item.function().0,
            span.file.0,
            span.range.start,
            span.range.end,
        )
        .expect("writing to a String cannot fail");
    }
    message
}

const fn invalid_root_error_code(code: InvalidRootCode) -> &'static str {
    match code {
        InvalidRootCode::UnknownEntry => "NativePreparationUnknownEntry",
        InvalidRootCode::InvalidFunction => "NativePreparationInvalidRootFunction",
        InvalidRootCode::DuplicateTest => "NativePreparationDuplicateTestRoot",
        InvalidRootCode::RootSignature => "NativePreparationRootSignature",
        InvalidRootCode::RootCapability => "NativePreparationRootCapability",
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
    CheckedMir {
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
            PreparedRoute::CheckedMir { .. } => NativeRouteKind::CheckedMir,
        }
    }
}

/// Prepares one immutable whole-artifact route and its exact LLVM target.
///
/// # Errors
///
/// Returns a structured invalid-root, unsupported, resource, target, or
/// compiler-defect error. Valid but unsupported LCIR selects one whole
/// checked-MIR plan only under [`NativeRoutePolicy::Automatic`].
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
        NativeRoutePolicy::Automatic | NativeRoutePolicy::LcirOnly => {
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
                LoweringOutcome::Unsupported(report) => {
                    trace_llvm_stage("prepare.lcir-lowering.unsupported");
                    if policy == NativeRoutePolicy::LcirOnly {
                        return Err(NativePreparationError::unsupported(report));
                    }
                    target
                        .validate_checked_mir_value_abi()
                        .map_err(|error| NativePreparationError::target(&error))?;
                    let (roots, reachable) =
                        checked_mir_graph_after_validated_lowering(mir, &options)?;
                    PreparedRoute::CheckedMir {
                        mir,
                        roots,
                        reachable,
                    }
                }
            }
        }
        NativeRoutePolicy::CheckedMirOnly => {
            let roots = validated_checked_mir_roots(mir, &options)?;
            let reachable = analyze_source_reachability(mir, &roots).map_err(|error| {
                NativePreparationError::defect(
                    "NativePreparationSourceGraphDefect",
                    format!("checked-MIR reachability failed: {error}"),
                )
            })?;
            target
                .validate_checked_mir_value_abi()
                .map_err(|error| NativePreparationError::target(&error))?;
            PreparedRoute::CheckedMir {
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
        PreparedRoute::CheckedMir {
            mir,
            roots,
            reachable,
        } => checked_mir_object_fingerprint_with_target(
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
        PreparedRoute::CheckedMir {
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

fn checked_mir_graph_after_validated_lowering(
    mir: &CheckedProgram,
    options: &EmitOptions,
) -> Result<(SourceRoots, ReachableSourceGraph), NativePreparationError> {
    let roots = raw_roots(mir, options).ok_or_else(|| {
        NativePreparationError::defect(
            "NativePreparationInconsistentPlan",
            "LCIR classification accepted a root which disappeared before checked-MIR preparation",
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

fn validated_checked_mir_roots(
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
    if let Err(error) = validate_checked_mir_root_signatures(mir, options, &roots) {
        let code = if error.code() == "InvalidFunctionReference" {
            "NativePreparationInvalidRootFunction"
        } else {
            "NativePreparationRootSignature"
        };
        return Err(NativePreparationError::invalid_root(code, error.message()));
    }
    Ok(roots)
}

fn raw_roots(mir: &CheckedProgram, options: &EmitOptions) -> Option<SourceRoots> {
    match &options.kind {
        EmitKind::Run { entry } => SourceRoots::for_entry(mir, entry),
        EmitKind::Tests => Some(SourceRoots::for_tests(mir)),
    }
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
            "loom-lcir-native-object-v41"
        );
    }
}
