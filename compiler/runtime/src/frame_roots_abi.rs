//! Private owner-scoped frame roots. The run callback spans the full native
//! owner/drain activation, not an individual coroutine resume. Root-set pointers
//! and IDs never escape that scope or cross threads; no worker receives them.
//! Managed values are allocation bases, null, or static values, never interiors.
//!
//! Reads/writes/insertion/removal do not collect or invoke user code. A read
//! result is only a snapshot: reload after any allocating call. To transfer a
//! result, root the receiver's copy before removing the producer's root.
//! ID and output pointers address valid, aligned native storage. Output pointers
//! are exclusive; read leaves its output untouched if the ID is stale.
//! The native callback does not unwind across the C ABI. This boundary supplies
//! neither a TaskFault catcher nor suspended cleanup.

use super::frame_roots::{FrameRootId, FrameRoots, with_frame_roots};
use std::ptr;

type Run = unsafe extern "C" fn(*mut u8, *const FrameRoots);

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_frame_roots_run(context: *mut u8, run: Run) {
    with_frame_roots(|roots| {
        // SAFETY: The caller retains native context storage and roots any of
        // its managed fields. The callback cannot retain roots beyond this call.
        unsafe { run(context, ptr::from_ref(roots)) };
    });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_frame_root_insert(
    roots: *const FrameRoots,
    value: *mut u8,
    out: *mut FrameRootId,
) {
    // SAFETY: The owner supplies a live root set and initialized managed base;
    // insertion does not collect, so value remains valid until it is registered.
    let id = unsafe { (&*roots).insert(value) };
    unsafe { out.write(id) };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_frame_root_read(
    roots: *const FrameRoots,
    id: *const FrameRootId,
    out: *mut *mut u8,
) -> u32 {
    // SAFETY: Root set and ID belong to the active owner scope.
    if let Some(value) = unsafe { (&*roots).get(*id) } {
        unsafe { out.write(value) };
        1
    } else {
        0
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_frame_root_write(
    roots: *const FrameRoots,
    id: *const FrameRootId,
    value: *mut u8,
) -> u32 {
    // SAFETY: The replacement is a live managed base (or null/static value),
    // and this noncollecting operation cannot invalidate the input snapshot.
    u32::from(unsafe { (&*roots).replace(*id, value) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_frame_root_remove(
    roots: *const FrameRoots,
    id: *const FrameRootId,
) -> u32 {
    // SAFETY: Root set and ID belong to the active owner scope. Any transferred
    // value has already been rooted by its receiver before this removal.
    u32::from(unsafe { (&*roots).remove(*id) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{loom_rt_box_new, loom_rt_collect};
    use std::mem::{MaybeUninit, size_of};

    unsafe extern "C" fn exercise(context: *mut u8, roots: *const FrameRoots) {
        let mut id = MaybeUninit::uninit();
        let value = loom_rt_box_new(8, None);
        // SAFETY: This callback has exclusive native output storage and a live
        // owner root set. The fresh allocation is initialized and never interior.
        unsafe {
            value.cast::<u64>().write(42);
            loom_rt_frame_root_insert(roots, value, id.as_mut_ptr());
            let id = id.assume_init();
            let previous = value as usize;
            loom_rt_collect();
            let mut moved = ptr::null_mut();
            assert_eq!(loom_rt_frame_root_read(roots, &id, &mut moved), 1);
            assert_ne!(moved as usize, previous);
            context.cast::<u64>().write(moved.cast::<u64>().read());
            assert_eq!(loom_rt_frame_root_write(roots, &id, ptr::null_mut()), 1);
            assert_eq!(loom_rt_frame_root_read(roots, &id, &mut moved), 1);
            assert!(moved.is_null());
            assert_eq!(loom_rt_frame_root_remove(roots, &id), 1);
            assert_eq!(loom_rt_frame_root_remove(roots, &id), 0);
            assert_eq!(loom_rt_frame_root_write(roots, &id, ptr::null_mut()), 0);
            assert_eq!(loom_rt_frame_root_read(roots, &id, &mut moved), 0);
        }
    }

    #[test]
    fn scoped_frame_root_abi_reloads_a_moved_payload() {
        assert_eq!(size_of::<FrameRootId>(), 16);
        let mut result = 0u64;
        // SAFETY: Native context is live throughout the callback; no root-set
        // pointer or ID escapes the owner scope.
        unsafe { loom_rt_frame_roots_run(ptr::from_mut(&mut result).cast(), exercise) };
        assert_eq!(result, 42);
        loom_rt_collect();
    }
}
