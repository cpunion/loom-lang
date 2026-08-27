use std::fmt::{self, Write};

use crate::{
    ArtifactKind, CheckedArtifact, DumpOptions, TestOutcomePlan, write_program_with_options,
};

/// Schema number for the compiler-private checked-artifact identity.
///
/// This is an invalidation boundary, not a compatibility promise. A change to
/// the encoded LCIR meaning must change this number before the identity is used
/// by a persistent object cache.
pub const ARTIFACT_IDENTITY_SCHEMA: u32 = 13;

/// Route tag which separates whole-artifact LCIR code generation from other
/// native object pipelines.
pub const ARTIFACT_IDENTITY_ROUTE: &str = "typed-lcir-whole-artifact";

/// Returns the deterministic compiler-private identity of a checked artifact.
///
/// The identity includes the artifact kind and ordered roots followed by the
/// canonical checked-LCIR dump with complete source origins. Dense numeric IDs
/// are content; the process-local generative program brand is not.
///
/// # Panics
///
/// Panics only if Rust's infallible [`String`] formatter reports an error.
#[must_use]
pub fn artifact_identity(artifact: &CheckedArtifact) -> String {
    let mut output = String::new();
    write_artifact_identity(artifact, &mut output)
        .expect("writing an artifact identity to a String cannot fail");
    output
}

/// Writes the deterministic compiler-private identity of a checked artifact.
///
/// The schema and route tags make this suitable as one input to a backend
/// object fingerprint. LCIR content reuses the single canonical dump encoder
/// rather than maintaining a second representation beside it.
///
/// # Errors
///
/// Returns only an error reported by the destination formatter.
pub fn write_artifact_identity(artifact: &CheckedArtifact, output: &mut impl Write) -> fmt::Result {
    writeln!(output, "loom-checked-artifact-identity")?;
    writeln!(output, "schema={ARTIFACT_IDENTITY_SCHEMA}")?;
    writeln!(output, "route={ARTIFACT_IDENTITY_ROUTE}")?;
    writeln!(output, "kind={}", artifact_kind(artifact.kind()))?;
    writeln!(output, "roots={}", artifact.roots().len())?;
    let outcomes = artifact.test_outcomes();
    for (index, root) in artifact.roots().iter().enumerate() {
        write!(output, "root[{index}]={}", root.raw())?;
        if let Some(outcome) = outcomes.and_then(|outcomes| outcomes.get(index)) {
            match outcome {
                TestOutcomePlan::Unit => write!(output, " outcome=unit")?,
                TestOutcomePlan::Result {
                    success_variant,
                    failure_variant,
                } => write!(
                    output,
                    " outcome=result success={success_variant} failure={failure_variant}"
                )?,
            }
        }
        writeln!(output)?;
    }
    writeln!(output, "payload=checked-lcir-with-origins")?;
    write_program_with_options(
        artifact.program(),
        DumpOptions {
            include_origins: true,
        },
        output,
    )
}

const fn artifact_kind(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Run => "run",
        ArtifactKind::Tests => "tests",
    }
}
