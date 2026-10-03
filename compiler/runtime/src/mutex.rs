//! Native acquisition for source scoped mutex guards. The source library owns
//! the public types and Dispose/MustScope/NoSuspend policy. Active guards retain
//! a native lock, not a managed pointer; their owner-local tokens never repeat.

use super::{HEAP, fatal, fault, rooted, shared_access::Access};
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

static NEXT: AtomicI64 = AtomicI64::new(1);

thread_local! {
    static GUARDS: RefCell<Vec<(i64, Arc<Access>)>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn watermark() -> i64 {
    NEXT.load(Ordering::Relaxed)
}

pub(super) fn acquired_since(mark: i64) -> bool {
    GUARDS.with(|guards| guards.borrow().iter().any(|(token, _)| *token >= mark))
}

/// Key is a live rooted allocation base. Acquisition may park and relocate the
/// heap. The caller reloads its snapshots and retains the returned obligation
/// in a lexical guard; no native lock finalizer substitutes for source cleanup.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_mutex_lock(key: *mut u8) -> i64 {
    rooted([key], |slots| {
        super::worker_control::checkpoint();
        // The checkpoint may relocate the caller's key.
        let key = unsafe { *slots };
        let access = HEAP.with(|heap| {
            let mut heap = heap.borrow_mut();
            if !heap.objects.contains_key(&(key as usize)) {
                fatal("mutex requires a managed identity");
            }
            Arc::clone(heap.mutexes.entry(key as usize).or_default())
        });
        if GUARDS.with(|guards| {
            guards
                .borrow()
                .iter()
                .any(|(_, held)| Arc::ptr_eq(held, &access))
        }) {
            fault("mutex already locked by this thread");
        }
        let token = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .unwrap_or_else(|_| fatal("mutex guard identities exhausted"));
        super::worker_control::acquire(&access);
        GUARDS.with(|guards| guards.borrow_mut().push((token, access)));
        token
    })
}

/// Dispose is idempotent: a stale or foreign-owner token cannot release a later
/// acquisition. Removing the token precedes release and invokes no user code/GC.
#[unsafe(no_mangle)]
pub(super) extern "C" fn loom_rt_mutex_unlock(token: i64) -> i32 {
    let access = GUARDS.with(|guards| {
        let mut guards = guards.borrow_mut();
        let index = guards.iter().position(|(id, _)| *id == token)?;
        Some(guards.swap_remove(index).1)
    });
    if let Some(access) = access {
        access.release();
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{loom_rt_box_new, loom_rt_collect};

    #[test]
    fn guards_keep_identity_through_gc_and_cannot_release_new_or_foreign_locks() {
        rooted([loom_rt_box_new(8, None)], |slots| unsafe {
            let first = loom_rt_mutex_lock(*slots);
            loom_rt_collect();
            let failure = crate::cleanup::catch_fault(|| loom_rt_mutex_lock(*slots)).unwrap_err();
            assert_eq!(failure.message, b"mutex already locked by this thread");
            assert_eq!(loom_rt_mutex_unlock(first), 1);
            let next = loom_rt_mutex_lock(*slots);
            assert!(next > first);
            assert_eq!(loom_rt_mutex_unlock(first), 0);
            assert_eq!(
                std::thread::spawn(move || loom_rt_mutex_unlock(next))
                    .join()
                    .unwrap(),
                0
            );
            assert_eq!(loom_rt_mutex_unlock(next), 1);
            assert_eq!(loom_rt_mutex_unlock(next), 0);
        });
        loom_rt_collect();
        assert!(HEAP.with(|heap| heap.borrow().mutexes.is_empty()));
    }
}
