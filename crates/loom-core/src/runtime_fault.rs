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

/// Stable runtime-fault code for a negative Task.sleep duration.
pub const INVALID_SLEEP_DURATION_FAULT_CODE: &str = "InvalidSleepDuration";

/// Stable user-facing message for a negative Task.sleep duration.
pub const INVALID_SLEEP_DURATION_FAULT_MESSAGE: &str = "sleep duration must not be negative";

/// Stable runtime-fault code for Task.sleep timer-range overflow.
pub const SLEEP_DURATION_OVERFLOW_FAULT_CODE: &str = "SleepDurationOverflow";

/// Stable user-facing message for Task.sleep timer-range overflow.
pub const SLEEP_DURATION_OVERFLOW_FAULT_MESSAGE: &str = "sleep duration overflowed";

/// Stable runtime-fault code emitted when Task.any has no successful child.
pub const TASK_ANY_FAILED_FAULT_CODE: &str = "TaskAnyFailed";

/// Stable user-facing message when Task.any has no successful child.
pub const TASK_ANY_FAILED_FAULT_MESSAGE: &str = "Task.any completed without a successful task";

/// Stable runtime-fault code emitted when a structured log line cannot be
/// written to the process error stream.
pub const LOG_WRITE_FAULT_CODE: &str = "LogWriteFault";

/// Stable user-facing message for a structured-log output failure.
pub const LOG_WRITE_FAULT_MESSAGE: &str = "log write failed";

/// Stable runtime-fault code emitted when a serialized construction proof
/// fails independent replay at an artifact trust boundary.
pub const ARTIFACT_PROOF_REJECTED_FAULT_CODE: &str = "ArtifactProofRejected";

/// Stable user-facing message for a rejected serialized construction proof.
pub const ARTIFACT_PROOF_REJECTED_FAULT_MESSAGE: &str =
    "serialized construction proof did not satisfy its predicate or invariant";
