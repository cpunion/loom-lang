//! Private cooperative mutator boundary. This coordinates moving collection,
//! not access to mutable object contents and not a source worker executor.
//! Native callbacks must obey the root/safe-point contract before sharing data.

use super::{Heap, ROOTS, RefCell, RootFrame, fatal};
use std::cell::Cell;
use std::collections::HashMap;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

struct Storage(RefCell<Heap>);

// SAFETY: The mutex exclusively protects allocator metadata. Managed pointers
// are accessed only by registered mutators; relocation and tracing run with
// every mutator parked. The last participant leaves before storage is dropped.
unsafe impl Send for Storage {}

#[derive(Default)]
struct Control {
    next: u64,
    // None means running; Some publishes that parked thread's root-chain head.
    // Addresses are never dereferenced without the complete stop-the-world
    // rendezvous. Only the owning thread can resume or remove its registration.
    participants: HashMap<u64, Option<usize>>,
    // Slot destruction removes its address under this mutex before freeing it.
    slots: HashMap<u64, usize>,
}

/// A queued/running/completed job retains one traced frame without borrowing
/// the owner's thread-local root store. The heap scope must drain these slots.
pub(super) struct SharedSlot {
    heap: *const SharedHeap,
    id: u64,
    value: AtomicPtr<u8>,
}

// SAFETY: The native slot outlives queued work. Its managed value may be read
// only by an attached mutator, and is rewritten only at the GC rendezvous.
unsafe impl Send for SharedSlot {}
unsafe impl Sync for SharedSlot {}

impl SharedSlot {
    pub(super) unsafe fn enter<R>(&self, run: impl FnOnce(*mut u8) -> R) -> R {
        if !ROOTS.get().is_null() || MEMORY.with(|memory| !memory.heap.borrow().objects.is_empty())
        {
            fatal("worker entered with a different live heap");
        }
        unsafe { &*self.heap }.attach(|| run(self.value.load(Ordering::Acquire)))
    }
}

impl Drop for SharedSlot {
    fn drop(&mut self) {
        // The scope checks for outstanding slots before dropping its heap.
        lock(&unsafe { &*self.heap }.control).slots.remove(&self.id);
    }
}

pub(super) unsafe fn publish(value: *mut u8) -> Arc<SharedSlot> {
    current(|participant| {
        let participant = participant.unwrap_or_else(|| fatal("worker requires a shared heap"));
        let mut control = participant.control();
        let id = control.next;
        control.next = id
            .checked_add(1)
            .unwrap_or_else(|| fatal("worker identities exhausted"));
        let slot = Arc::new(SharedSlot {
            heap: ptr::from_ref(participant.heap),
            id,
            value: AtomicPtr::new(value),
        });
        control.slots.insert(id, Arc::as_ptr(&slot) as usize);
        slot
    })
}

unsafe extern "C" fn trace_slots(address: *mut u8) {
    let slots = unsafe { &*address.cast::<HashMap<u64, usize>>() };
    for address in slots.values() {
        // The control mutex excludes removal/free, even if the last native job
        // starts dropping a slot concurrently. Never promote a weak Arc here:
        // dropping that temporary last owner would reenter the same mutex.
        let slot = unsafe { &*(*address as *const SharedSlot) };
        slot.value.store(
            super::loom_rt_visit(slot.value.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
    }
}

pub(super) struct SharedHeap {
    storage: Mutex<Storage>,
    control: Mutex<Control>,
    changed: Condvar,
    requested: AtomicBool,
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Running,
    Parked,
    Collecting,
}

struct Participant<'a> {
    heap: &'a SharedHeap,
    id: u64,
    state: Cell<State>,
    entry_roots: *mut RootFrame,
    entry_mutexes: i64,
}

struct Memory {
    heap: RefCell<Heap>,
    // Points into the active attach invocation, never another native thread.
    participant: Cell<*const Participant<'static>>,
}

thread_local! {
    // One TLS lookup selects the local heap or shared participant. Ordinary
    // allocations must not pay a second TLS lookup just to reject shared mode.
    static MEMORY: Memory = Memory {
        heap: RefCell::new(Heap::default()),
        participant: Cell::new(ptr::null()),
    };
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|_| fatal("poisoned shared heap"))
}

fn current<R>(run: impl FnOnce(Option<&Participant<'_>>) -> R) -> R {
    MEMORY.with(|memory| {
        // SAFETY: attach owns this stack record for the whole native activation.
        // The reference never escapes this call; the slot is thread-local.
        run(unsafe { memory.participant.get().as_ref() })
    })
}

pub(super) fn with_heap<R>(run: impl FnOnce(&RefCell<Heap>) -> R) -> R {
    // SAFETY: As in current(), attach retains the participant for this complete
    // activation. No heap borrow or reference survives this callback.
    MEMORY.with(
        |memory| match unsafe { memory.participant.get().as_ref() } {
            None => run(&memory.heap),
            Some(participant) => {
                if participant.state.get() == State::Parked {
                    fatal("managed heap access while mutator is parked");
                }
                run(&lock(&participant.heap.storage).0)
            }
        },
    )
}

impl SharedHeap {
    fn wait<'a>(&'a self, control: MutexGuard<'a, Control>) -> MutexGuard<'a, Control> {
        self.changed
            .wait(control)
            .unwrap_or_else(|_| fatal("poisoned shared heap rendezvous"))
    }

    fn attach<R>(&self, run: impl FnOnce() -> R) -> R {
        if active() {
            fatal("nested shared heap attachment");
        }
        let mut control = lock(&self.control);
        while self.requested.load(Ordering::Relaxed) {
            control = self.wait(control);
        }
        let id = control.next;
        control.next = id
            .checked_add(1)
            .unwrap_or_else(|| fatal("mutator identities exhausted"));
        control.participants.insert(id, None);
        drop(control);
        let participant = Participant {
            heap: self,
            id,
            state: Cell::new(State::Running),
            entry_roots: ROOTS.get(),
            entry_mutexes: super::mutex::watermark(),
        };
        // Erase only the stored raw pointer's lifetime. Its stack owner cannot
        // return before the guard removes it; nothing can send it away.
        MEMORY.with(|memory| memory.participant.set(ptr::from_ref(&participant).cast()));
        let result = run();
        drop(participant);
        result
    }
}

impl Drop for Participant<'_> {
    fn drop(&mut self) {
        if self.state.get() != State::Running
            || ROOTS.get() != self.entry_roots
            || !super::shared_access::idle()
            || super::mutex::acquired_since(self.entry_mutexes)
        {
            fatal("unbalanced mutator exit");
        }
        // A collector may be waiting for this thread. Removing a root-free
        // registration is also a rendezvous; no managed access follows it.
        let mut control = lock(&self.heap.control);
        control.participants.remove(&self.id);
        MEMORY.with(|memory| memory.participant.set(ptr::null()));
        self.heap.changed.notify_all();
    }
}

impl Participant<'_> {
    fn control(&self) -> MutexGuard<'_, Control> {
        if self.state.get() != State::Running {
            fatal("nested mutator safe point");
        }
        lock(&self.heap.control)
    }
}

fn park(participant: &Participant<'_>, control: &mut Control) {
    if participant.state.replace(State::Parked) != State::Running {
        fatal("nested mutator safe point");
    }
    *control.participants.get_mut(&participant.id).unwrap() = Some(ROOTS.get() as usize);
    participant.heap.changed.notify_all();
}

fn resume<'a>(participant: &Participant<'a>, mut control: MutexGuard<'a, Control>) {
    let heap = participant.heap;
    while heap.requested.load(Ordering::Relaxed) {
        control = heap.wait(control);
    }
    *control.participants.get_mut(&participant.id).unwrap() = None;
    participant.state.set(State::Running);
}

pub(super) fn collect(run: impl FnOnce(&[*mut RootFrame])) {
    current(|participant| {
        let Some(participant) = participant else {
            return run(&[ROOTS.get()]);
        };
        let heap = participant.heap;
        let mut control = participant.control();
        park(participant, &mut control);
        if heap.requested.load(Ordering::Relaxed) {
            // Another collector covers this request. Publishing our roots lets
            // it finish instead of waiting while holding an allocator borrow.
            return resume(participant, control);
        }
        heap.requested.store(true, Ordering::Release);
        participant.state.set(State::Collecting);
        while control.participants.values().any(Option::is_none) {
            control = heap.wait(control);
        }
        let mut roots: Vec<_> = control
            .participants
            .values()
            .map(|head| head.unwrap() as *mut RootFrame)
            .collect();
        let entry = super::Root {
            address: ptr::from_ref(&control.slots).cast_mut().cast(),
            trace: trace_slots,
        };
        let mut frame = RootFrame {
            previous: ptr::null_mut(),
            roots: &entry,
            count: 1,
        };
        roots.push(ptr::from_mut(&mut frame));
        // No participant can resume or attach while this lock is held. Heap
        // borrows remain short; a generated tracer may reenter loom_rt_visit.
        run(&roots);
        heap.requested.store(false, Ordering::Release);
        *control.participants.get_mut(&participant.id).unwrap() = None;
        participant.state.set(State::Running);
        heap.changed.notify_all();
    });
}

/// Compiler safe point: all live managed snapshots are in updateable roots;
/// reload them after returning, even if no allocation occurred on this thread.
#[unsafe(no_mangle)]
pub(super) extern "C" fn loom_rt_shared_checkpoint() {
    current(|participant| {
        let Some(participant) = participant else {
            return;
        };
        let heap = participant.heap;
        if heap.requested.load(Ordering::Acquire) {
            let mut control = participant.control();
            park(participant, &mut control);
            resume(participant, control);
        }
    });
}

type Run = unsafe extern "C" fn(*mut u8);
type SharedRun = unsafe extern "C" fn(*mut u8, *const SharedHeap);

pub(super) fn active() -> bool {
    MEMORY.with(|memory| !memory.participant.get().is_null())
}

pub(super) fn park_native<R>(run: impl FnOnce() -> R) -> R {
    current(|participant| {
        let Some(participant) = participant else {
            return run();
        };
        let heap = participant.heap;
        let mut control = participant.control();
        park(participant, &mut control);
        drop(control);
        let result = run();
        resume(participant, lock(&heap.control));
        result
    })
}

/// Establish one shared heap scope, adopting this thread's existing objects.
/// The callback must join all participating native threads before returning.
/// It catches language faults internally and balances its native roots.
/// Context is native storage; managed fields need registered updateable roots.
#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_shared_run(context: *mut u8, run: SharedRun) {
    with_shared(|heap| unsafe { run(context, heap) });
}

pub(super) fn with_shared<R>(run: impl FnOnce(*const SharedHeap) -> R) -> R {
    current(|participant| {
        if let Some(participant) = participant {
            // A nested structured scope reuses its current heap, not a copy.
            return run(ptr::from_ref(participant.heap));
        }
        let heap = SharedHeap {
            storage: Mutex::new(Storage(RefCell::new(
                MEMORY.with(|memory| std::mem::take(&mut *memory.heap.borrow_mut())),
            ))),
            control: Mutex::new(Control::default()),
            changed: Condvar::new(),
            requested: AtomicBool::new(false),
        };
        let result = heap.attach(|| run(ptr::from_ref(&heap)));
        let control = lock(&heap.control);
        if !control.participants.is_empty() || !control.slots.is_empty() {
            fatal("shared heap scope returned before its workers");
        }
        drop(control);
        let storage = heap
            .storage
            .into_inner()
            .unwrap_or_else(|_| fatal("poisoned shared heap"));
        MEMORY.with(|memory| *memory.heap.borrow_mut() = storage.0.into_inner());
        result
    })
}

/// Attach a native worker to a still-live shared scope. The worker's local heap
/// and root chain must be empty. No managed argument is read before attachment;
/// acquire values from rooted shared handoff storage inside the callback.
#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_shared_enter(heap: *const SharedHeap, context: *mut u8, run: Run) {
    if !ROOTS.get().is_null() || MEMORY.with(|memory| !memory.heap.borrow().objects.is_empty()) {
        fatal("worker entered with a different live heap");
    }
    // SAFETY: The shared scope outlives this native thread's joined activation.
    unsafe { &*heap }.attach(|| unsafe { run(context) });
}

/// Park across a native blocking wait. The callback may use only independent
/// native data, never managed pointers, roots, or generated Loom code. It must
/// return normally. Rooted managed snapshots must be reloaded after this call.
#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_shared_park(context: *mut u8, run: Run) {
    park_native(|| unsafe { run(context) });
}

#[cfg(test)]
#[path = "shared_heap_tests.rs"]
mod tests;
