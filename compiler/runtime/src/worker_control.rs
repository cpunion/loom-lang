//! Cooperative interruption of one native Loom worker activation. Cancellation
//! wakes a blocked source mutex without acquiring it; cleanup runs on the worker
//! with live roots, after resuming its mutator. Internal storage guards are not
//! interruption points in the middle of a primitive.

use crate::{cleanup, fatal, shared_access::Access};
use std::cell::Cell;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Control {
    started: AtomicBool,
    requested: AtomicBool,
    waiting: Mutex<Option<Arc<Access>>>,
}

#[derive(Debug)]
pub enum Exit {
    Cancelled,
    Fault {
        message: Vec<u8>,
        test_name: Option<Vec<u8>>,
    },
}

thread_local! {
    static CURRENT: Cell<*const Control> = const { Cell::new(ptr::null()) };
    static MASKED: Cell<bool> = const { Cell::new(false) };
}

struct Activation;

impl Drop for Activation {
    fn drop(&mut self) {
        CURRENT.set(ptr::null());
    }
}

struct Mask(bool);

impl Drop for Mask {
    fn drop(&mut self) {
        MASKED.set(self.0);
    }
}

pub(super) fn mask() -> impl Drop {
    Mask(MASKED.replace(true))
}

impl Control {
    /// Request interruption, including a currently blocked mutex acquisition.
    /// Returning does not mean cleanup has finished: the owner must still drain.
    pub fn cancel(&self) {
        self.requested.store(true, Ordering::Release);
        let waiting = self.waiting.lock().unwrap().clone();
        if let Some(access) = waiting {
            access.wake();
        }
    }

    /// Run exactly one activation. A request before entry skips the callback;
    /// completion after the last checkpoint wins over a late request.
    ///
    /// # Safety
    /// The caller obeys the runtime fault/root ABI: all generated frames unwind,
    /// cleanup captures remain live, and no abandoned root has a Rust destructor.
    /// Any shared managed data additionally requires an attached mutator and
    /// compiler-instrumented safe points/accesses. This does not attach a heap.
    pub unsafe fn run<R>(&self, run: impl FnOnce() -> R) -> Result<R, Exit> {
        if self.started.swap(true, Ordering::Relaxed) || !CURRENT.get().is_null() {
            fatal("worker control requires one unnested activation");
        }
        CURRENT.set(ptr::from_ref(self));
        let _activation = Activation;
        // SAFETY: The caller provides the live-stack fault contract above.
        unsafe {
            cleanup::catch_worker(|| {
                checkpoint();
                run()
            })
        }
        .map_err(|failure| match failure {
            cleanup::Failure::Cancelled => Exit::Cancelled,
            cleanup::Failure::Fault(fault) => Exit::Fault {
                message: fault.message,
                test_name: fault.test_name,
            },
        })
    }

    pub(super) fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    pub(super) fn wait_on(&self, access: Option<Arc<Access>>) {
        *self.waiting.lock().unwrap() = access;
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self) -> bool {
        self.waiting.lock().unwrap().is_some()
    }
}

fn current<R>(run: impl FnOnce(Option<&Control>) -> R) -> R {
    // SAFETY: run retains this control through the entire activation and clears
    // the slot on unwind. This reference is private and never leaves this module.
    run(unsafe { CURRENT.get().as_ref() }.filter(|_| !MASKED.get()))
}

pub(super) fn checkpoint() {
    current(|control| {
        if control.is_some_and(Control::requested) {
            cleanup::cancel();
        }
    });
}

pub(super) fn acquire(access: &Arc<Access>) {
    current(|control| {
        if let Some(control) = control {
            if !access.acquire_cancellable(control) {
                // acquire has resumed the mutator and dropped all native borrows.
                cleanup::cancel();
            }
        } else {
            access.acquire();
        }
    });
}

/// All managed snapshots must be rooted and reloaded across this call, including
/// scalar loops that do not allocate. Cancellation is never a TaskFault string.
#[unsafe(no_mangle)]
extern "C-unwind" fn loom_rt_worker_checkpoint() {
    crate::shared_heap::loom_rt_shared_checkpoint();
    checkpoint();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::{Cleanup, catch_fault};
    use std::mem::MaybeUninit;

    unsafe extern "C" {
        fn loom_rt_cleanup_push(
            record: *mut Cleanup,
            callback: unsafe extern "C-unwind" fn(*mut u8),
            captures: *mut u8,
        );
    }

    unsafe extern "C-unwind" fn clean(data: *mut u8) {
        // Checkpoints inside mandatory cleanup must not interrupt the drain.
        loom_rt_worker_checkpoint();
        unsafe { (*data.cast::<Vec<u8>>()).push(1) };
    }

    unsafe extern "C-unwind" fn fail(data: *mut u8) {
        unsafe { (*data.cast::<Vec<u8>>()).push(2) };
        cleanup::fault(b"cleanup failed");
    }

    #[test]
    fn cancellation_skips_unstarted_work_and_preserves_completed_results() {
        let control = Control::default();
        control.cancel();
        assert!(matches!(
            unsafe { control.run(|| panic!("cancelled callback ran")) },
            Err(Exit::Cancelled)
        ));
        let control = Control::default();
        assert_eq!(unsafe { control.run(|| 42) }.unwrap(), 42);
        control.cancel();
        // No worker context remains on this thread after completion.
        loom_rt_worker_checkpoint();
    }

    #[test]
    fn cancellation_crosses_nested_catchers_and_retains_cleanup_faults() {
        for cleanup_fault in [false, true] {
            let control = Control::default();
            let mut events = Vec::<u8>::new();
            let data = ptr::from_mut(&mut events).cast();
            let result = unsafe {
                control.run(|| {
                    let mut outer = MaybeUninit::uninit();
                    loom_rt_cleanup_push(outer.as_mut_ptr(), clean, data);
                    let nested = catch_fault::<()>(|| {
                        let mut inner = MaybeUninit::uninit();
                        loom_rt_cleanup_push(
                            inner.as_mut_ptr(),
                            if cleanup_fault { fail } else { clean },
                            data,
                        );
                        control.cancel();
                        loom_rt_worker_checkpoint();
                        panic!("cancellation returned");
                    });
                    // A nested catcher may return an actual cleanup fault; it
                    // cannot turn cancellation into a catchable diagnostic.
                    cleanup::raise_owned(nested.unwrap_err());
                })
            };
            if cleanup_fault {
                let Err(Exit::Fault { message, .. }) = result else {
                    panic!("cleanup fault lost: {result:?}");
                };
                assert_eq!(message, b"cleanup failed");
                assert_eq!(events, [2, 1]);
            } else {
                assert!(matches!(result, Err(Exit::Cancelled)));
                assert_eq!(events, [1, 1]);
            }
            assert!(crate::ROOTS.get().is_null());
            loom_rt_worker_checkpoint();
        }
    }
}
