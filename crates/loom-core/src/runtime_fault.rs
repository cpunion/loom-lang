//! Stable language-level runtime-fault vocabulary.
//!
//! Fault codes and their canonical messages are shared by every execution
//! backend. They belong to the language diagnostic boundary, not to checked
//! MIR or the compiler-private native runtime ABI.

/// Stable runtime-fault code for checked signed-integer arithmetic overflow.
pub const INTEGER_OVERFLOW_FAULT_CODE: &str = "IntegerOverflow";

/// Stable user-facing message for checked signed-integer arithmetic overflow.
pub const INTEGER_OVERFLOW_FAULT_MESSAGE: &str = "integer arithmetic overflowed";
