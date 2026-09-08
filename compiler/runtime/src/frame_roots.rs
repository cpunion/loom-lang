//! Typed frame pointers rooted for one native owner's complete run/drain scope.
//! Identities belong to that owner, not to other scopes or worker threads.
//! Root operations never collect; callers reload pointers after each safepoint.

use super::{
    HEAP, Root, RootFrame, fatal, loom_rt_roots_enter, loom_rt_roots_leave, loom_rt_visit,
};
use std::cell::RefCell;
use std::mem::MaybeUninit;
use std::ptr;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FrameRootId {
    pub key: u64,
    pub generation: u64,
}

struct Entry {
    slot: usize,
    pointer: *mut u8,
}

struct Slot {
    generation: u64,
    position: Option<usize>,
}

#[derive(Default)]
struct Store {
    active: Vec<Entry>,
    slots: Vec<Slot>,
    free: Vec<usize>,
}

impl Store {
    fn position(&self, id: FrameRootId) -> Option<usize> {
        let slot = self.slots.get(usize::try_from(id.key).ok()?)?;
        if slot.generation == id.generation {
            slot.position
        } else {
            None
        }
    }
}

// Raw managed pointers keep this owner !Send; RefCell also makes it !Sync.
pub(super) struct FrameRoots {
    store: RefCell<Store>,
}

fn outside_tracing() {
    if HEAP.with(|heap| heap.borrow().collecting) {
        fatal("frame roots cannot change during GC tracing");
    }
}

impl FrameRoots {
    /// Retain a frame until removal or owner-scope exit.
    ///
    /// # Safety
    /// The pointer is a live allocation base in this thread's Loom heap, null,
    /// or static storage. It is never a stack or managed interior pointer.
    pub(super) unsafe fn insert(&self, pointer: *mut u8) -> FrameRootId {
        outside_tracing();
        let mut store = self.store.borrow_mut();
        let index = store.free.last().copied().unwrap_or(store.slots.len());
        let generation = store
            .slots
            .get(index)
            .map_or(0, |slot| slot.generation)
            .checked_add(1)
            .unwrap_or_else(|| fatal("frame root generation exhausted"));
        let key = u64::try_from(index).unwrap_or_else(|_| fatal("frame root identities exhausted"));
        let position = store.active.len();
        store.active.push(Entry {
            slot: index,
            pointer,
        });
        if index == store.slots.len() {
            store.slots.push(Slot {
                generation,
                position: Some(position),
            });
        } else {
            store.free.pop();
            store.slots[index] = Slot {
                generation,
                position: Some(position),
            };
        }
        FrameRootId { key, generation }
    }

    pub(super) fn get(&self, id: FrameRootId) -> Option<*mut u8> {
        let store = self.store.borrow();
        Some(store.active[store.position(id)?].pointer)
    }

    /// Replace a retained pointer without changing its identity.
    ///
    /// # Safety
    /// The new pointer has the same validity requirements as `insert`.
    pub(super) unsafe fn replace(&self, id: FrameRootId, pointer: *mut u8) -> bool {
        outside_tracing();
        let mut store = self.store.borrow_mut();
        let Some(position) = store.position(id) else {
            return false;
        };
        store.active[position].pointer = pointer;
        true
    }

    pub(super) fn remove(&self, id: FrameRootId) -> bool {
        outside_tracing();
        let mut store = self.store.borrow_mut();
        let Some(position) = store.position(id) else {
            return false;
        };
        let removed = store.active.swap_remove(position);
        if position < store.active.len() {
            let moved = store.active[position].slot;
            store.slots[moved].position = Some(position);
        }
        store.slots[removed.slot].position = None;
        store.free.push(removed.slot);
        true
    }
}

unsafe extern "C" fn trace(address: *mut u8) {
    // SAFETY: The owner and its shared RefCell remain live throughout the scope.
    let roots = unsafe { &*address.cast::<FrameRoots>() };
    let mut store = roots.store.borrow_mut();
    for entry in &mut store.active {
        // visit rewrites the base and queues payload tracing. It invokes no
        // generated tracer while this Store borrow is held.
        entry.pointer = loom_rt_visit(entry.pointer);
    }
}

struct ActiveFrame(*mut RootFrame);

impl Drop for ActiveFrame {
    fn drop(&mut self) {
        outside_tracing();
        // SAFETY: Scope locals outlive this guard, including during Rust unwind.
        // Nested root scopes must also honor LIFO on exit.
        unsafe { loom_rt_roots_leave(self.0) };
    }
}

pub(super) fn with_frame_roots<R>(run: impl FnOnce(&FrameRoots) -> R) -> R {
    outside_tracing();
    let roots = FrameRoots {
        store: RefCell::new(Store::default()),
    };
    let entry = Root {
        address: ptr::from_ref(&roots).cast_mut().cast(),
        trace,
    };
    let mut frame = MaybeUninit::<RootFrame>::uninit();
    let frame_pointer = frame.as_mut_ptr();
    // SAFETY: These locals keep fixed addresses until the guard leaves. Only
    // the owner is registered, so growing/reordering its Vec cannot stale roots.
    unsafe { loom_rt_roots_enter(frame_pointer, ptr::from_ref(&entry), 1) };
    let _active = ActiveFrame(frame_pointer);
    run(&roots)
}
