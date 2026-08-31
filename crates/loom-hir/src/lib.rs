//! Source-independent high-level IR for loom-lang.
//!
//! Syntax nodes are deliberately not annotated with semantic information. This
//! crate owns the stable, typed identities and arenas used by later compiler
//! phases, while `loom-sema` owns resolution and type side tables.

mod arena;
mod ids;
mod ir;
mod lower;
mod source_map;

pub use arena::{Arena, ArenaId, ArenaMap};
pub use ids::{
    BodyId, DefId, ExprId, GenericParamId, LocalId, ModuleId, ParamId, PatternId, TypeRefId,
};
pub use ir::*;
pub use lower::{
    LoweringResult, PackageSourceMode, PackageSourceUnit, SelectedPackageSourceUnit, SourceUnit,
    lower_files, lower_package_files, lower_selected_package_files,
};
pub use source_map::{BodySourceMap, ProgramSourceMap};
