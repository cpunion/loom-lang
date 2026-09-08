//! Private fault capture for one synchronous coroutine resume activation.
//! The native owner calls this inside its outer frame-root scope. Captured
//! diagnostics own Rust bytes, not GC pointers; release them after materializing
//! the task outcome. This is not a general FFI or foreign-exception catcher.

use super::cleanup::{OwnedFault, catch_fault};
use std::ptr;

type Resume = unsafe extern "C-unwind" fn(*mut u8);

#[repr(C)]
struct FaultView {
    message: *const u8,
    message_len: usize,
    test_name: *const u8,
    test_name_len: usize,
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_fault_boundary(context: *mut u8, resume: Resume) -> *mut OwnedFault {
    // SAFETY: The native owner observes catch_fault's live-stack contract. All
    // generated callees permit unwinding; the private marker is caught here.
    // No managed field in context is used without an independent, current root.
    match unsafe { catch_fault(|| resume(context)) } {
        Ok(()) => ptr::null_mut(),
        Err(fault) => Box::into_raw(Box::new(fault)),
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_fault_read(fault: *const OwnedFault, out: *mut FaultView) {
    // SAFETY: A nonnull handle came from boundary and remains owned by the
    // caller. out is exclusive aligned native storage. The view borrows bytes
    // until drop; reading it does not collect or invoke generated code.
    let fault = unsafe { &*fault };
    let (test_name, test_name_len) = fault
        .test_name
        .as_ref()
        .map_or((ptr::null(), 0), |name| (name.as_ptr(), name.len()));
    unsafe {
        out.write(FaultView {
            message: fault.message.as_ptr(),
            message_len: fault.message.len(),
            test_name,
            test_name_len,
        });
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_fault_drop(fault: *mut OwnedFault) {
    // SAFETY: The caller releases its nonnull handle exactly once, after all
    // borrowed views have expired. Deallocation neither collects nor runs Loom.
    unsafe { drop(Box::from_raw(fault)) };
}
