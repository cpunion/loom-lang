//! Compiler identities for the four Task composition intrinsics.
//!
//! Semantic analysis admits an intrinsic only after lexical, value, import,
//! generic, and type shadowing have been excluded. This file is deleted when
//! Task composition can be expressed by ordinary source definitions.

use loom_core::Name;

use crate::TaskIntrinsic;

#[must_use]
pub(crate) fn resolve(member: &Name) -> Option<TaskIntrinsic> {
    match member.as_str() {
        "all" => Some(TaskIntrinsic::All),
        "settled" => Some(TaskIntrinsic::Settled),
        "any" => Some(TaskIntrinsic::Any),
        "race" => Some(TaskIntrinsic::Race),
        _ => None,
    }
}
