//! Private per-object access guards for compiler-generated storage operations.
//! This is not a source lock or implicit atomicity for an entire function.
//! Object roots retain the lock identity across relocation and blocked entry.

use super::{AddressHasher, HEAP, Objects, fatal, rooted, shared_heap};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::sync::{Arc, Condvar, Mutex, TryLockError};

#[derive(Default)]
pub(super) struct Access {
    held: Mutex<bool>,
    changed: Condvar,
}

pub(super) type Locks = HashMap<usize, Arc<Access>, BuildHasherDefault<AddressHasher>>;

thread_local! {
    static HELD: RefCell<Vec<Arc<Access>>> = const { RefCell::new(Vec::new()) };
}

impl Access {
    fn try_acquire(&self) -> bool {
        match self.held.try_lock() {
            Ok(mut held) if !*held => {
                *held = true;
                true
            }
            Ok(_) | Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Poisoned(_)) => fatal("poisoned storage access"),
        }
    }

    fn acquire(&self) {
        if self.try_acquire() {
            return;
        }
        // No allocator borrow or managed pointer access crosses the wait.
        // GC can move the protected object while this thread is parked. Drop
        // the native state mutex before resuming to avoid lock-order inversion.
        shared_heap::park_native(|| {
            let mut held = self
                .held
                .lock()
                .unwrap_or_else(|_| fatal("poisoned storage access"));
            while *held {
                held = self
                    .changed
                    .wait(held)
                    .unwrap_or_else(|_| fatal("poisoned storage wait"));
            }
            *held = true;
        });
    }

    fn release(&self) {
        *self
            .held
            .lock()
            .unwrap_or_else(|_| fatal("poisoned storage access")) = false;
        self.changed.notify_one();
    }
}

pub(super) fn idle() -> bool {
    HELD.with(|held| held.borrow().is_empty())
}

pub(super) fn release_all() {
    while !idle() {
        loom_rt_shared_access_end();
    }
}

pub(super) fn relocate(locks: &mut Locks, previous: &Objects) {
    if locks.is_empty() {
        return;
    }
    let old = std::mem::take(locks);
    for (address, access) in old {
        if let Some(object) = previous.get(&address) {
            if !object.forwarded.is_null() {
                locks.insert(object.forwarded as usize, access);
            }
        }
    }
}

/// Acquire one managed object's access guard. The caller roots all live values
/// and reloads them afterward: contended entry is a moving-GC safe point.
/// Target is an allocation base. Balance with access_end before user callbacks
/// or returning; nested internal operations on the same object are reentrant.
/// Multiple distinct objects require a consistent caller-established lock order.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn loom_rt_shared_access_begin(target: *mut u8) {
    if !shared_heap::active() {
        return;
    }
    rooted([target], |_| {
        let access = HEAP.with(|heap| {
            let mut heap = heap.borrow_mut();
            if !heap.objects.contains_key(&(target as usize)) {
                fatal("storage access requires a managed allocation base");
            }
            Arc::clone(heap.access.entry(target as usize).or_default())
        });
        let reentrant =
            HELD.with(|held| held.borrow().iter().any(|item| Arc::ptr_eq(item, &access)));
        if !reentrant {
            access.acquire();
        }
        HELD.with(|held| held.borrow_mut().push(access));
    });
}

/// Release the innermost internal guard. Does not collect or run user code.
#[unsafe(no_mangle)]
pub(super) extern "C" fn loom_rt_shared_access_end() {
    if !shared_heap::active() {
        return;
    }
    let release = HELD.with(|held| {
        let mut held = held.borrow_mut();
        let access = held
            .pop()
            .unwrap_or_else(|| fatal("unbalanced storage access"));
        if held.iter().any(|item| Arc::ptr_eq(item, &access)) {
            None
        } else {
            Some(access)
        }
    });
    if let Some(access) = release {
        access.release();
    }
}
