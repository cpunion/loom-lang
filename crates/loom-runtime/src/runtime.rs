//! Process-local managed runtime state shared by synchronous code and, when
//! needed, one attached async executor.

use std::ffi::c_void;
use std::ptr;

use crate::gc::LoomHeap;
use crate::{WAIT_INVALID_ARGUMENT, WAIT_OK};

/// Opaque owner of Loom's managed heap.
///
/// The C ABI always allocates this object in a `Box`, so its address is stable
/// until a successful `loom_runtime_destroy_v1`. An executor borrows that
/// stable address; the attachment marker prevents two schedulers from driving
/// the same heap at once.
pub struct LoomRuntime {
    pub(crate) heap: LoomHeap,
    attached_executor: *mut c_void,
}

impl LoomRuntime {
    pub(crate) fn new() -> Self {
        Self {
            heap: LoomHeap::default(),
            attached_executor: ptr::null_mut(),
        }
    }

    pub(crate) fn try_attach_executor(&mut self, executor: *mut c_void) -> bool {
        if executor.is_null() || !self.attached_executor.is_null() {
            return false;
        }
        self.attached_executor = executor;
        true
    }

    pub(crate) fn detach_executor(&mut self, executor: *mut c_void) {
        debug_assert_eq!(self.attached_executor, executor);
        if self.attached_executor == executor {
            self.attached_executor = ptr::null_mut();
        }
    }

    /// Checks the opaque identity of the one executor attached to this
    /// runtime without dereferencing the candidate pointer.
    pub(crate) fn is_attached_executor(&self, executor: *mut c_void) -> bool {
        !executor.is_null() && self.attached_executor == executor
    }

    fn has_attached_executor(&self) -> bool {
        !self.attached_executor.is_null()
    }
}

#[unsafe(export_name = "loom_runtime_create_v1")]
pub extern "C" fn runtime_create_v1() -> *mut LoomRuntime {
    Box::into_raw(Box::new(LoomRuntime::new()))
}

/// Destroys a detached runtime.
///
/// Returning a status lets the ABI reject an accidental destroy while an
/// executor still borrows the runtime instead of creating a dangling pointer.
#[unsafe(export_name = "loom_runtime_destroy_v1")]
pub unsafe extern "C" fn runtime_destroy_v1(runtime: *mut LoomRuntime) -> i32 {
    if runtime.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    // SAFETY: the pointer is non-null and the ABI requires it to originate
    // from runtime_create_v1. We inspect it before taking back ownership.
    if unsafe { (*runtime).has_attached_executor() } || crate::gc::runtime_is_active(runtime) {
        return WAIT_INVALID_ARGUMENT;
    }
    // SAFETY: no executor is attached, so ownership can return to this call.
    drop(unsafe { Box::from_raw(runtime) });
    WAIT_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::{allocate_value, allocate_value_node};
    use crate::reactor::{executor_create_for_runtime_v1, executor_destroy};

    #[test]
    fn null_runtime_operations_fail_without_side_effects() {
        unsafe {
            assert_eq!(runtime_destroy_v1(ptr::null_mut()), WAIT_INVALID_ARGUMENT);
        }
    }

    #[test]
    fn detached_runtime_can_be_destroyed() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn standalone_runtime_owns_allocations_without_an_executor() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert!(!(*runtime).has_attached_executor());
            assert_eq!(crate::gc::activate_runtime_v1(runtime), WAIT_OK);
            assert!(!allocate_value().is_null());
            assert!(!allocate_value_node().is_null());
            assert_eq!(crate::gc::deactivate_runtime_v1(runtime), WAIT_OK);
            assert_eq!((*runtime).heap.values.len(), 1);
            assert_eq!((*runtime).heap.nodes.len(), 1);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn borrowed_executor_attachment_is_exclusive_and_reusable_after_drop() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            let executor = executor_create_for_runtime_v1(runtime);
            assert!(!executor.is_null());
            assert_eq!((*executor).runtime_pointer(), runtime);
            assert!(executor_create_for_runtime_v1(runtime).is_null());
            assert_eq!(runtime_destroy_v1(runtime), WAIT_INVALID_ARGUMENT);

            executor_destroy(executor);
            let replacement = executor_create_for_runtime_v1(runtime);
            assert!(!replacement.is_null());
            executor_destroy(replacement);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn activation_nests_only_for_the_same_runtime() {
        let first = runtime_create_v1();
        let second = runtime_create_v1();
        assert!(!first.is_null() && !second.is_null());
        unsafe {
            assert_eq!(crate::gc::activate_runtime_v1(first), WAIT_OK);
            assert_eq!(crate::gc::activate_runtime_v1(first), WAIT_OK);
            assert_eq!(
                crate::gc::activate_runtime_v1(second),
                WAIT_INVALID_ARGUMENT,
            );
            assert_eq!(
                crate::gc::deactivate_runtime_v1(second),
                WAIT_INVALID_ARGUMENT,
            );
            assert_eq!(runtime_destroy_v1(first), WAIT_INVALID_ARGUMENT);
            assert_eq!(crate::gc::deactivate_runtime_v1(first), WAIT_OK);
            assert_eq!(crate::gc::deactivate_runtime_v1(first), WAIT_OK);
            assert_eq!(crate::gc::activate_runtime_v1(second), WAIT_OK);
            assert_eq!(crate::gc::deactivate_runtime_v1(second), WAIT_OK);
            assert_eq!(runtime_destroy_v1(first), WAIT_OK);
            assert_eq!(runtime_destroy_v1(second), WAIT_OK);
        }
    }
}
