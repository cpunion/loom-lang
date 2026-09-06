//! Native object emission from checked programs, independent of the backend.

use crate::model::checked;
use std::path::Path;

mod llvm;
pub use llvm::Llvm;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Optimization {
    O0,
    O1,
    #[default]
    O2,
    O3,
}

pub struct EmitOptions<'a> {
    pub object: &'a Path,
    /// Optional inspection output in the selected backend's own IR format.
    pub ir: Option<&'a Path>,
    pub test_mode: bool,
    pub optimization: Optimization,
}

pub struct EmissionResult {
    pub library: bool,
    pub uses_runtime: bool,
}

/// Emit an object preserving checked operations, faults, and the runtime ABI.
/// Linking and publication belong to the host tool, not the code generator.
pub trait Backend {
    fn emit(
        &self,
        program: &checked::Program,
        options: EmitOptions<'_>,
    ) -> Result<EmissionResult, String>;
}
