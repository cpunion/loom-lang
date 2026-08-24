//! Total, deterministic lowering from error-free typed HIR to executable MIR.
//!
//! This crate is the compiler's final trusted translation boundary. It never
//! returns a partially executable program: missing semantic facts, unsupported
//! typed shapes, or invalid MIR are reported as structured compiler defects.

mod lower;

pub use lower::{LoweringFailure, lower_to_mir};
