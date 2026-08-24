//! Native runtime linked into LLVM-produced Loom executables.
//!
//! The scheduler, structured Task tree, joins, cancellation and managed frame
//! relocation are ordinary Rust. Unsafe code is confined to the private C ABI
//! and the one-shot raw-fd registration whose lifetime is represented by a
//! runtime registration entry.

#![deny(unsafe_op_in_unsafe_fn)]
// This crate is a compiler-private C ABI. Its exported unsafe entry points are
// documented at the ABI level, and the boxed vectors intentionally provide
// stable addresses for pointers held by generated code.
#![allow(
    clippy::missing_safety_doc,
    clippy::must_use_candidate,
    clippy::struct_excessive_bools,
    clippy::vec_box
)]

mod float;
mod gc;
mod int;
mod process;
mod reactor;
mod scheduler;

pub use reactor::{
    LoomReadyNotification, LoomRegistration, LoomWaitSource, executor_cancel, executor_create,
    executor_destroy, executor_last_os_error, executor_notify_completion, executor_pop_ready,
    executor_register, executor_wait, wait_fd_once, wait_now_ns,
};
pub use scheduler::{
    LoomJoinSpec, LoomTask, executor_gc_collections, executor_gc_live_objects,
    executor_gc_reclaimed, executor_gc_relocations, executor_run, join_add_list, join_add_task,
    join_create, join_task, task_add_join_child, task_cancel, task_from_wait_source,
    task_is_cancelled, task_join_count, task_join_result, task_join_result_step, task_join_step,
    task_join_winner, task_prepare_join, task_result, task_set_state, task_slot, task_spawn,
    task_suspend_join, task_suspend_value, task_suspend_wait, task_write_join_result,
};

pub const WAIT_ABI_VERSION: u32 = 1;
pub const WAIT_INFINITE: u64 = u64::MAX;

pub const TASK_COMPLETED: i32 = 0;
pub const TASK_PENDING: i32 = 1;
pub const TASK_FAULTED: i32 = 2;
pub const TASK_CANCELLED: i32 = 3;

pub const TASK_JOIN_ALL: u32 = 0;
pub const TASK_JOIN_SETTLED: u32 = 1;
pub const TASK_JOIN_ANY: u32 = 2;
pub const TASK_JOIN_RACE: u32 = 3;

pub const WAIT_OK: i32 = 0;
pub const WAIT_INVALID_ARGUMENT: i32 = 1;
pub const WAIT_UNSUPPORTED: i32 = 2;
pub const WAIT_SYSTEM_ERROR: i32 = 3;
pub const WAIT_DUPLICATE_SOURCE: i32 = 4;
pub const WAIT_STALE_REGISTRATION: i32 = 5;
pub const WAIT_NO_MEMORY: i32 = 6;

pub const WAIT_SOURCE_TIMER: u32 = 1;
pub const WAIT_SOURCE_FD: u32 = 2;
pub const WAIT_SOURCE_COMPLETION: u32 = 3;

pub const WAIT_READABLE: u32 = 1 << 0;
pub const WAIT_WRITABLE: u32 = 1 << 1;

pub const READY_READABLE: u32 = 1 << 0;
pub const READY_WRITABLE: u32 = 1 << 1;
pub const READY_TIMER: u32 = 1 << 2;
pub const READY_COMPLETED: u32 = 1 << 3;
pub const READY_CLOSED: u32 = 1 << 4;
pub const READY_ERROR: u32 = 1 << 5;

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::reactor::LoomExecutor;

    fn source(kind: u32) -> LoomWaitSource {
        LoomWaitSource {
            abi_version: WAIT_ABI_VERSION,
            kind,
            handle: -1,
            ..LoomWaitSource::default()
        }
    }

    static VALUE_RELOCATED: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn gc_fixture_resume(
        task: *mut LoomTask,
        executor: *mut LoomExecutor,
    ) -> i32 {
        let slot = unsafe { task_slot(task, 0).cast::<scheduler::ValueSlot>() };
        match unsafe { scheduler::task_state(task) } {
            0 => {
                let _unreachable = gc::allocate_value();
                let live = gc::allocate_value().cast::<scheduler::ValueSlot>();
                unsafe {
                    (*slot).words[0] = 7;
                    (*slot).words[3] = live as u64;
                    (*slot).words[4] = live as u64;
                }
                let wait = LoomWaitSource {
                    abi_version: WAIT_ABI_VERSION,
                    kind: WAIT_SOURCE_TIMER,
                    handle: -1,
                    deadline_ns: wait_now_ns().saturating_add(1_000_000),
                    ..LoomWaitSource::default()
                };
                if unsafe { task_suspend_wait(executor, task, &raw const wait) } != WAIT_OK {
                    return TASK_FAULTED;
                }
                unsafe { task_set_state(task, 1) };
                TASK_PENDING
            }
            1 => {
                let old = unsafe { (*slot).words[3] };
                let current = unsafe { (*slot).words[4] };
                VALUE_RELOCATED.store(old != current, Ordering::SeqCst);
                unsafe { slot.write(scheduler::ValueSlot::default()) };
                TASK_COMPLETED
            }
            _ => TASK_FAULTED,
        }
    }

    #[test]
    fn real_completion_timer_and_fd_are_one_shot() {
        let executor = executor_create();
        assert!(!executor.is_null());
        let mut frame = 1_i32;
        let frame_pointer = (&raw mut frame).cast::<c_void>();

        let completion = source(WAIT_SOURCE_COMPLETION);
        let mut registration = LoomRegistration::default();
        // SAFETY: all stack values outlive their one-shot registrations.
        unsafe {
            assert_eq!(
                executor_register(
                    executor,
                    &raw const completion,
                    frame_pointer,
                    &raw mut registration,
                ),
                WAIT_OK
            );
            assert_eq!(
                executor_notify_completion(executor, &raw const registration, 0, 0),
                WAIT_OK
            );
            let mut ready_count = 0;
            assert_eq!(executor_wait(executor, 0, &raw mut ready_count), WAIT_OK);
            assert_eq!(ready_count, 1);
            let mut ready = LoomReadyNotification::default();
            assert_eq!(executor_pop_ready(executor, &raw mut ready), 1);
            assert_eq!(ready.frame, frame_pointer);
            assert_ne!(ready.events & READY_COMPLETED, 0);
            assert_eq!(
                executor_notify_completion(executor, &raw const registration, 0, 0),
                WAIT_STALE_REGISTRATION
            );

            let mut timer = source(WAIT_SOURCE_TIMER);
            timer.deadline_ns = wait_now_ns() + 1_000_000;
            assert_eq!(
                executor_register(
                    executor,
                    &raw const timer,
                    frame_pointer,
                    &raw mut registration,
                ),
                WAIT_OK
            );
            assert_eq!(
                executor_wait(executor, 2_000_000_000, &raw mut ready_count),
                WAIT_OK
            );
            assert_eq!(executor_pop_ready(executor, &raw mut ready), 1);
            assert_ne!(ready.events & READY_TIMER, 0);

            let (left, mut right) = UnixStream::pair().expect("create socket pair");
            left.set_nonblocking(true).expect("make socket nonblocking");
            let mut readable = source(WAIT_SOURCE_FD);
            readable.handle = i64::from(left.as_raw_fd());
            readable.interests = WAIT_READABLE;
            assert_eq!(
                executor_register(
                    executor,
                    &raw const readable,
                    frame_pointer,
                    &raw mut registration,
                ),
                WAIT_OK
            );
            right.write_all(b"x").expect("make socket readable");
            assert_eq!(
                executor_wait(executor, 2_000_000_000, &raw mut ready_count),
                WAIT_OK
            );
            assert_eq!(executor_pop_ready(executor, &raw mut ready), 1);
            assert_ne!(ready.events & READY_READABLE, 0);

            executor_destroy(executor);
        }
    }

    #[test]
    fn moving_heap_rewrites_roots_and_reclaims_unreachable_values() {
        VALUE_RELOCATED.store(false, Ordering::SeqCst);
        let executor = executor_create();
        assert!(!executor.is_null());
        let task = unsafe { task_spawn(executor, Some(gc_fixture_resume), 1, 0) };
        assert!(!task.is_null());
        assert_eq!(unsafe { executor_run(executor, task) }, TASK_COMPLETED);
        assert!(VALUE_RELOCATED.load(Ordering::SeqCst));
        assert!(unsafe { executor_gc_relocations(executor) } >= 1);
        assert!(unsafe { executor_gc_reclaimed(executor) } >= 1);
        // The completed callback cleared its last object root. A final
        // scheduler safepoint proves the survivor can be reclaimed too.
        unsafe { gc::collect(&mut *executor) };
        assert_eq!(unsafe { executor_gc_live_objects(executor) }, 0);
        unsafe { executor_destroy(executor) };
    }

    #[test]
    fn many_one_shot_completion_registrations_drain_exactly_once() {
        const COUNT: usize = 512;
        let executor = executor_create();
        assert!(!executor.is_null());
        let completion = source(WAIT_SOURCE_COMPLETION);
        let mut frames = (0..COUNT)
            .map(|index| Box::new(i32::try_from(index).expect("frame index fits i32")))
            .collect::<Vec<_>>();
        let mut registrations = vec![LoomRegistration::default(); COUNT];
        unsafe {
            for (frame, registration) in frames.iter_mut().zip(&mut registrations) {
                assert_eq!(
                    executor_register(
                        executor,
                        &raw const completion,
                        (&raw mut **frame).cast::<c_void>(),
                        registration,
                    ),
                    WAIT_OK
                );
            }
            for registration in &registrations {
                assert_eq!(
                    executor_notify_completion(executor, registration, 0, 0),
                    WAIT_OK
                );
                assert_eq!(
                    executor_notify_completion(executor, registration, 0, 0),
                    WAIT_STALE_REGISTRATION
                );
            }
            let mut ready_count = 0;
            assert_eq!(executor_wait(executor, 0, &raw mut ready_count), WAIT_OK);
            assert_eq!(
                usize::try_from(ready_count).expect("ready count fits usize"),
                COUNT
            );
            let mut seen = std::collections::BTreeSet::new();
            for _ in 0..COUNT {
                let mut ready = LoomReadyNotification::default();
                assert_eq!(executor_pop_ready(executor, &raw mut ready), 1);
                assert_ne!(ready.events & READY_COMPLETED, 0);
                assert!(seen.insert(ready.frame as usize));
            }
            assert_eq!(seen.len(), COUNT);
            let mut empty = LoomReadyNotification::default();
            assert_eq!(executor_pop_ready(executor, &raw mut empty), 0);
            executor_destroy(executor);
        }
    }
}
