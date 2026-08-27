//! Stable language-level runtime-fault vocabulary.
//!
//! Fault codes and their canonical messages are shared by every execution
//! backend. They belong to the language diagnostic boundary, not to checked
//! MIR or the compiler-private native runtime ABI.

/// Stable runtime-fault code for checked signed-integer arithmetic overflow.
pub const INTEGER_OVERFLOW_FAULT_CODE: &str = "IntegerOverflow";

/// Stable user-facing message for checked signed-integer arithmetic overflow.
pub const INTEGER_OVERFLOW_FAULT_MESSAGE: &str = "integer arithmetic overflowed";

/// Stable runtime-fault code for a negative Duration construction.
pub const INVALID_DURATION_FAULT_CODE: &str = "InvalidDuration";

/// Stable user-facing message for a negative Duration construction.
pub const INVALID_DURATION_FAULT_MESSAGE: &str = "Duration milliseconds cannot be negative";

/// Stable runtime-fault code emitted when a serialized construction proof
/// fails independent replay at an artifact trust boundary.
pub const ARTIFACT_PROOF_REJECTED_FAULT_CODE: &str = "ArtifactProofRejected";

/// Stable user-facing message for a rejected serialized construction proof.
pub const ARTIFACT_PROOF_REJECTED_FAULT_MESSAGE: &str =
    "serialized construction proof did not satisfy its predicate or invariant";
