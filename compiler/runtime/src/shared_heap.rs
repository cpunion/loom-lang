//! Private cooperative mutator boundary. This coordinates moving collection,
//! not access to mutable object contents and not a source worker executor.
//! Native callbacks must obey the root/safe-point contract before sharing data.

use super::{Heap, LOCAL_HEAP, ROOTS, RefCell, RootFrame, fatal};
use std::cell::Cell;
use std::collections::HashMap;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};

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
}

thread_local! {
    // Points into the active attach invocation, never another native thread.
    static CURRENT: Cell<*const Participant<'static>> = const { Cell::new(ptr::null()) };
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|_| fatal("poisoned shared heap"))
}

fn current<R>(run: impl FnOnce(Option<&Participant<'_>>) -> R) -> R {
    let pointer = CURRENT.get();
    // SAFETY: attach owns this stack record for the whole native activation.
    // The reference never escapes this call; CURRENT is private to this thread.
    run(unsafe { pointer.as_ref() })
}

pub(super) fn with_heap<R>(run: impl FnOnce(&RefCell<Heap>) -> R) -> R {
    current(|participant| match participant {
        None => LOCAL_HEAP.with(run),
        Some(participant) => {
            if participant.state.get() == State::Parked {
                fatal("managed heap access while mutator is parked");
            }
            run(&lock(&participant.heap.storage).0)
        }
    })
}

impl SharedHeap {
    fn wait<'a>(&'a self, control: MutexGuard<'a, Control>) -> MutexGuard<'a, Control> {
        self.changed
            .wait(control)
            .unwrap_or_else(|_| fatal("poisoned shared heap rendezvous"))
    }

    fn attach<R>(&self, run: impl FnOnce() -> R) -> R {
        if !CURRENT.get().is_null() {
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
        };
        // Erase only the stored raw pointer's lifetime. Its stack owner cannot
        // return before the guard removes CURRENT; nothing can send it away.
        CURRENT.set(ptr::from_ref(&participant).cast());
        let result = run();
        drop(participant);
        result
    }
}

impl Drop for Participant<'_> {
    fn drop(&mut self) {
        if self.state.get() != State::Running || ROOTS.get() != self.entry_roots {
            fatal("unbalanced mutator exit");
        }
        // A collector may be waiting for this thread. Removing a root-free
        // registration is also a rendezvous; no managed access follows it.
        let mut control = lock(&self.heap.control);
        control.participants.remove(&self.id);
        CURRENT.set(ptr::null());
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
        let roots: Vec<_> = control
            .participants
            .values()
            .map(|head| head.unwrap() as *mut RootFrame)
            .collect();
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
extern "C" fn loom_rt_shared_checkpoint() {
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

/// Establish one shared heap scope, adopting this thread's existing objects.
/// The callback must join all participating native threads before returning.
/// It catches language faults internally and balances its native roots.
/// Context is native storage; managed fields need registered updateable roots.
#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_shared_run(context: *mut u8, run: SharedRun) {
    current(|participant| {
        if let Some(participant) = participant {
            // A nested structured scope reuses its current heap, not a copy.
            unsafe { run(context, ptr::from_ref(participant.heap)) };
            return;
        }
        let heap = SharedHeap {
            storage: Mutex::new(Storage(RefCell::new(
                LOCAL_HEAP.with(|heap| std::mem::take(&mut *heap.borrow_mut())),
            ))),
            control: Mutex::new(Control::default()),
            changed: Condvar::new(),
            requested: AtomicBool::new(false),
        };
        heap.attach(|| unsafe { run(context, ptr::from_ref(&heap)) });
        if !lock(&heap.control).participants.is_empty() {
            fatal("shared heap scope returned before its workers");
        }
        let storage = heap
            .storage
            .into_inner()
            .unwrap_or_else(|_| fatal("poisoned shared heap"));
        LOCAL_HEAP.with(|heap| *heap.borrow_mut() = storage.0.into_inner());
    });
}

/// Attach a native worker to a still-live shared scope. The worker's local heap
/// and root chain must be empty. No managed argument is read before attachment;
/// acquire values from rooted shared handoff storage inside the callback.
#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_shared_enter(heap: *const SharedHeap, context: *mut u8, run: Run) {
    if !ROOTS.get().is_null() || LOCAL_HEAP.with(|heap| !heap.borrow().objects.is_empty()) {
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
    current(|participant| {
        let Some(participant) = participant else {
            return unsafe { run(context) };
        };
        let heap = participant.heap;
        let mut control = participant.control();
        park(participant, &mut control);
        drop(control);
        unsafe { run(context) };
        resume(participant, lock(&heap.control));
    });
}

#[cfg(test)]
#[path = "shared_heap_tests.rs"]
mod tests;
