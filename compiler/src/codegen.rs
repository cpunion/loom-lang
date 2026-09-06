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
    pub ir: Option<&'a Path>,
    pub test_mode: bool,
    pub optimization: Optimization,
}

pub struct EmissionResult {
    pub library: bool,
    pub uses_runtime: bool,
}

pub trait Backend {
    fn emit(
        &self,
        program: &checked::Program,
        options: EmitOptions<'_>,
    ) -> Result<EmissionResult, String>;
}
