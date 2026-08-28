//! Embedded identities for compiler-known standard-library API items.
//!
//! This versioned catalog is the version 0.3 bridge while Task composition is
//! not yet declared in source modules. Semantic analysis admits a catalog item
//! only after lexical, value, import, generic, and type shadowing have been
//! excluded. Later phases see only its stable identity. When the standard
//! library moves to source modules, trusted `DefId`s can map into this catalog
//! without changing type rules or MIR.

use loom_core::Name;
use loom_hir::Path;

use crate::StandardLibraryItem;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StandardNamespace {
    Task,
}

#[must_use]
pub(crate) fn resolve_namespace(path: &Path) -> Option<StandardNamespace> {
    let [segment] = path.segments.as_slice() else {
        return None;
    };
    match segment.name.as_str() {
        "Task" => Some(StandardNamespace::Task),
        _ => None,
    }
}

#[must_use]
pub(crate) fn resolve_static_item(
    namespace: StandardNamespace,
    member: &Name,
) -> Option<StandardLibraryItem> {
    match (namespace, member.as_str()) {
        (StandardNamespace::Task, "sleep") => Some(StandardLibraryItem::TaskSleep),
        (StandardNamespace::Task, "all") => Some(StandardLibraryItem::TaskAll),
        (StandardNamespace::Task, "settled") => Some(StandardLibraryItem::TaskSettled),
        (StandardNamespace::Task, "any") => Some(StandardLibraryItem::TaskAny),
        (StandardNamespace::Task, "race") => Some(StandardLibraryItem::TaskRace),
        _ => None,
    }
}
