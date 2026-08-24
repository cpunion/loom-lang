//! Name resolution, type checking, contracts, concepts, and lexical views.
//!
//! Semantic facts are stored beside HIR rather than written into syntax or HIR
//! nodes. This keeps partially erroneous programs useful to diagnostics and a
//! future LSP while ensuring executable MIR is produced only from a fully
//! verified program.

mod analyze;
mod conformance;
mod def_map;
mod module_graph;
mod proof;
mod resolver;
mod side_tables;
mod ty;

pub use analyze::{Analysis, analyze, analyze_reusing_bodies};
pub use conformance::*;
pub use def_map::{Binding, DefMap, DefMapBuild, Namespace};
pub use module_graph::{ImportEdge, ModuleGraph, ModuleGraphBuild};
pub use resolver::{ResolveError, Resolver};
pub use side_tables::*;
pub use ty::*;
