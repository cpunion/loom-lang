use super::*;
use crate::{HEAP, Root, loom_rt_collect, loom_rt_text_new, rooted, text_bytes};
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicUsize};
use std::thread;

// Native test adapters use the same C boundaries as generated callbacks. None
// retains an unrooted managed snapshot across attach, park or a checkpoint.
fn scope<F: FnOnce(*const SharedHeap)>(run: F) {
    unsafe extern "C" fn invoke<F: FnOnce(*const SharedHeap)>(
        context: *mut u8,
        heap: *const SharedHeap,
    ) {
        unsafe { &mut *context.cast::<Option<F>>() }.take().unwrap()(heap);
    }
    let mut run = Some(run);
    unsafe { loom_rt_shared_run(ptr::from_mut(&mut run).cast(), invoke::<F>) };
}

unsafe fn attach<F: FnOnce()>(heap: usize, run: F) {
    unsafe extern "C" fn invoke<F: FnOnce()>(context: *mut u8) {
        unsafe { &mut *context.cast::<Option<F>>() }.take().unwrap()();
    }
    let mut run = Some(run);
    unsafe {
        loom_rt_shared_enter(
            heap as *const SharedHeap,
            ptr::from_mut(&mut run).cast(),
            invoke::<F>,
        )
    };
}

fn parked<F: FnOnce()>(run: F) {
    unsafe extern "C" fn invoke<F: FnOnce()>(context: *mut u8) {
        unsafe { &mut *context.cast::<Option<F>>() }.take().unwrap()();
    }
    let mut run = Some(run);
    unsafe { loom_rt_shared_park(ptr::from_mut(&mut run).cast(), invoke::<F>) };
}

struct Handoff {
    input: AtomicPtr<u8>,
    outputs: [AtomicPtr<u8>; 2],
}

unsafe extern "C" fn trace_handoff(address: *mut u8) {
    // This native object contains UnsafeCell-backed atomic slots, not managed
    // interiors. The collector alone rewrites them while all mutators park.
    let handoff = unsafe { &*address.cast::<Handoff>() };
    for slot in [&handoff.input, &handoff.outputs[0], &handoff.outputs[1]] {
        slot.store(
            crate::loom_rt_visit(slot.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
    }
}

fn with_handoff(input: *mut u8, run: impl FnOnce(Arc<Handoff>)) {
    let handoff = Arc::new(Handoff {
        input: AtomicPtr::new(input),
        outputs: std::array::from_fn(|_| AtomicPtr::new(ptr::null_mut())),
    });
    let root = Root {
        address: Arc::as_ptr(&handoff).cast_mut().cast(),
        trace: trace_handoff,
    };
    let mut frame = MaybeUninit::uninit();
    unsafe { crate::loom_rt_roots_enter(frame.as_mut_ptr(), &root, 1) };
    run(Arc::clone(&handoff));
    unsafe { crate::loom_rt_roots_leave(frame.as_mut_ptr()) };
}

#[test]
fn parallel_collectors_rewrite_shared_aliases_and_return_worker_allocations() {
    loom_rt_collect();
    let before = HEAP.with(|heap| heap.borrow().objects.len());
    let input = unsafe { loom_rt_text_new(b"shared".as_ptr(), 6) };
    with_handoff(input, |handoff| {
        scope(|heap| {
            HEAP.with(|heap| heap.borrow_mut().stress = true);
            loom_rt_collect();
            assert_ne!(handoff.input.load(Ordering::Acquire), input);
            let barrier = Arc::new(std::sync::Barrier::new(3));
            let mut workers = Vec::new();
            for index in 0..2 {
                let heap = heap as usize;
                let barrier = Arc::clone(&barrier);
                let handoff = Arc::clone(&handoff);
                workers.push(thread::spawn(move || unsafe {
                    attach(heap, || {
                        let input = handoff.input.load(Ordering::Acquire);
                        rooted([input], |slots| {
                            parked(|| {
                                barrier.wait();
                            });
                            for _ in 0..32 {
                                let value = loom_rt_text_new(b"result".as_ptr(), 6);
                                handoff.outputs[index].store(value, Ordering::Release);
                                loom_rt_collect();
                                assert_eq!(*slots, handoff.input.load(Ordering::Acquire));
                                assert_eq!(text_bytes(*slots), b"shared");
                            }
                        });
                    });
                }));
            }
            parked(|| {
                barrier.wait();
                for worker in workers {
                    worker.join().unwrap();
                }
            });
            HEAP.with(|heap| heap.borrow_mut().stress = false);
        });
        // The initiating thread regains the same logical heap, including
        // worker-created objects. Collection must still visit those objects.
        loom_rt_collect();
        for output in &handoff.outputs {
            assert_eq!(
                unsafe { text_bytes(output.load(Ordering::Acquire)) },
                b"result"
            );
        }
    });
    loom_rt_collect();
    assert_eq!(HEAP.with(|heap| heap.borrow().objects.len()), before);
}

#[test]
fn nonallocating_mutator_checkpoints_allow_collection_and_reload_roots() {
    let input = unsafe { loom_rt_text_new(b"checkpoint".as_ptr(), 10) };
    with_handoff(input, |handoff| {
        scope(|heap| {
            let heap = heap as usize;
            let handoff = Arc::clone(&handoff);
            let ready = Arc::new(AtomicBool::new(false));
            let done = Arc::new(AtomicBool::new(false));
            let polls = Arc::new(AtomicUsize::new(0));
            let worker = {
                let ready = Arc::clone(&ready);
                let done = Arc::clone(&done);
                let polls = Arc::clone(&polls);
                thread::spawn(move || unsafe {
                    attach(heap, || {
                        rooted([handoff.input.load(Ordering::Acquire)], |slots| {
                            ready.store(true, Ordering::Release);
                            while !done.load(Ordering::Acquire) {
                                loom_rt_shared_checkpoint();
                                assert_eq!(*slots, handoff.input.load(Ordering::Acquire));
                                assert_eq!(text_bytes(*slots), b"checkpoint");
                                polls.fetch_add(1, Ordering::Relaxed);
                                thread::yield_now();
                            }
                        });
                    });
                })
            };
            while !ready.load(Ordering::Acquire) {
                thread::yield_now();
            }
            for _ in 0..16 {
                loom_rt_collect();
            }
            done.store(true, Ordering::Release);
            parked(|| worker.join().unwrap());
            assert!(polls.load(Ordering::Relaxed) > 0);
        });
    });
    loom_rt_collect();
}

#[test]
fn worker_fault_restores_only_its_own_root_chain() {
    let input = unsafe { loom_rt_text_new(b"parent".as_ptr(), 6) };
    with_handoff(input, |handoff| {
        scope(|heap| {
            let heap = heap as usize;
            let child_handoff = Arc::clone(&handoff);
            let worker = thread::spawn(move || unsafe {
                attach(heap, || {
                    let failure = crate::cleanup::catch_fault(|| {
                        rooted([child_handoff.input.load(Ordering::Acquire)], |slots| {
                            crate::shared_access::loom_rt_shared_access_begin(*slots);
                            loom_rt_collect();
                            crate::fault("worker failure");
                        });
                    })
                    .unwrap_err();
                    assert_eq!(failure.message, b"worker failure");
                    assert!(crate::shared_access::idle());
                    assert!(ROOTS.get().is_null());
                    loom_rt_collect();
                });
            });
            parked(|| worker.join().unwrap());
            assert!(!ROOTS.get().is_null());
            assert_eq!(
                unsafe { text_bytes(handoff.input.load(Ordering::Acquire)) },
                b"parent"
            );
        });
    });
    loom_rt_collect();
}

#[test]
fn storage_guards_allow_lost_updates_but_protect_explicit_compound_updates() {
    use crate::shared_access::{
        loom_rt_shared_access_begin as enter, loom_rt_shared_access_end as leave,
    };
    // This native fixture spells out the individual access boundaries that a
    // worker backend must emit; it is not evidence of a source worker API.
    for compound in [false, true] {
        let values = crate::loom_rt_list_new(size_of::<i64>(), None, 1);
        unsafe { crate::list_push(values, ptr::from_ref(&0i64).cast()) };
        with_handoff(values, |handoff| {
            scope(|heap| {
                let barrier = Arc::new(std::sync::Barrier::new(2));
                let mut workers = Vec::new();
                for _ in 0..2 {
                    let heap = heap as usize;
                    let barrier = Arc::clone(&barrier);
                    let handoff = Arc::clone(&handoff);
                    workers.push(thread::spawn(move || unsafe {
                        attach(heap, || {
                            rooted([handoff.input.load(Ordering::Acquire)], |slots| {
                                enter(*slots);
                                let value =
                                    *(*(*slots).cast::<crate::List>()).buffer.data.cast::<i64>();
                                if !compound {
                                    leave();
                                    parked(|| {
                                        barrier.wait();
                                    });
                                    enter(*slots);
                                }
                                *(*(*slots).cast::<crate::List>()).buffer.data.cast::<i64>() =
                                    value + 1;
                                leave();
                            });
                        });
                    }));
                }
                parked(|| {
                    for worker in workers {
                        worker.join().unwrap();
                    }
                });
                let values = handoff.input.load(Ordering::Acquire).cast::<crate::List>();
                assert_eq!(
                    unsafe { *(*values).buffer.data.cast::<i64>() },
                    if compound { 2 } else { 1 }
                );
            });
        });
    }
    loom_rt_collect();
    assert!(HEAP.with(|heap| heap.borrow().access.is_empty()));
}

#[test]
fn contended_storage_growth_parks_waiters_and_preserves_lock_identity_through_gc() {
    use crate::shared_access::{
        loom_rt_shared_access_begin as enter, loom_rt_shared_access_end as leave,
    };
    let values = crate::loom_rt_list_new(size_of::<i64>(), None, 0);
    with_handoff(values, |handoff| {
        scope(|heap| {
            HEAP.with(|heap| heap.borrow_mut().stress = true);
            let mut workers = Vec::new();
            for item in [1i64, 2] {
                let heap = heap as usize;
                let handoff = Arc::clone(&handoff);
                workers.push(thread::spawn(move || unsafe {
                    attach(heap, || {
                        rooted([handoff.input.load(Ordering::Acquire)], |slots| {
                            for _ in 0..32 {
                                enter(*slots);
                                // Nested native work on the same object must
                                // not deadlock, including after relocation.
                                loom_rt_collect();
                                enter(*slots);
                                crate::list_push(*slots, ptr::from_ref(&item).cast());
                                leave();
                                leave();
                            }
                        });
                    });
                }));
            }
            parked(|| {
                for worker in workers {
                    worker.join().unwrap();
                }
            });
            let values = handoff.input.load(Ordering::Acquire).cast::<crate::List>();
            unsafe {
                assert_eq!((*values).buffer.len, 64);
                let items = std::slice::from_raw_parts((*values).buffer.data.cast::<i64>(), 64);
                assert_eq!(items.iter().filter(|item| **item == 1).count(), 32);
                assert_eq!(items.iter().filter(|item| **item == 2).count(), 32);
            }
            HEAP.with(|heap| heap.borrow_mut().stress = false);
        });
    });
    loom_rt_collect();
    assert!(HEAP.with(|heap| heap.borrow().access.is_empty()));
}

#[test]
fn independent_objects_do_not_share_an_execution_lock() {
    use crate::shared_access::{
        loom_rt_shared_access_begin as enter, loom_rt_shared_access_end as leave,
    };
    with_handoff(ptr::null_mut(), |handoff| {
        scope(|heap| {
            for output in &handoff.outputs {
                output.store(crate::loom_rt_box_new(8, None), Ordering::Release);
            }
            let (arrived, ready) = std::sync::mpsc::channel();
            let mut release = Vec::new();
            let mut workers = Vec::new();
            for index in 0..2 {
                let heap = heap as usize;
                let handoff = Arc::clone(&handoff);
                let arrived = arrived.clone();
                let (send, receive) = std::sync::mpsc::channel();
                release.push(send);
                workers.push(thread::spawn(move || unsafe {
                    attach(heap, || {
                        rooted([handoff.outputs[index].load(Ordering::Acquire)], |slots| {
                            enter(*slots);
                            arrived.send(()).unwrap();
                            parked(|| receive.recv().unwrap());
                            (*slots).cast::<u64>().write(42);
                            leave();
                        });
                    });
                }));
            }
            let mut independent = false;
            parked(|| {
                independent = (0..2).all(|_| {
                    ready
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .is_ok()
                });
                // Release and join even if a regression serialized the locks,
                // so failure reports instead of leaving blocked native threads.
                for send in release {
                    send.send(()).unwrap();
                }
                for worker in workers {
                    worker.join().unwrap();
                }
            });
            assert!(independent);
            for output in &handoff.outputs {
                assert_eq!(
                    unsafe { output.load(Ordering::Acquire).cast::<u64>().read() },
                    42
                );
            }
        });
    });
    loom_rt_collect();
}

thread_local! {
    static NOTIFY: RefCell<Option<std::sync::mpsc::Sender<crate::native::Completion>>> = const { RefCell::new(None) };
    static COLLECTED: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static RESUMED_AFTER_COLLECTION: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "C-unwind" fn after_collection(_: *mut u8) -> i64 {
    if let Some(completion) = crate::native::wait_completion(81, false) {
        NOTIFY.with(|notify| {
            notify
                .borrow_mut()
                .take()
                .unwrap()
                .send(completion)
                .unwrap()
        });
        return 1;
    }
    RESUMED_AFTER_COLLECTION
        .set(COLLECTED.with(|value| value.borrow().as_ref().unwrap().load(Ordering::Acquire)));
    0
}

unsafe extern "C-unwind" fn collection_task() -> u64 {
    let frame = crate::loom_rt_box_new(8, None);
    unsafe { crate::tasks::loom_rt_task_create(frame, after_collection, ptr::null(), 0) }
}

#[test]
fn idle_task_owner_parks_until_a_collecting_mutator_publishes_completion() {
    scope(|heap| {
        let heap = heap as usize;
        let (send, receive) = std::sync::mpsc::channel::<crate::native::Completion>();
        let collected = Arc::new(AtomicBool::new(false));
        NOTIFY.with(|notify| *notify.borrow_mut() = Some(send));
        COLLECTED.with(|value| *value.borrow_mut() = Some(Arc::clone(&collected)));
        RESUMED_AFTER_COLLECTION.set(false);
        let worker = thread::spawn(move || unsafe {
            attach(heap, || {
                let completion = park_native(|| receive.recv().unwrap());
                let fallback = crate::native::Completion {
                    reactor: Arc::clone(&completion.reactor),
                    registration: completion.registration,
                };
                let (done, wait) = std::sync::mpsc::channel();
                let watchdog = thread::spawn(move || {
                    if wait
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .is_err()
                    {
                        // Break a regression's wait cycle so the assertion
                        // below can fail after all native threads have joined.
                        fallback.notify();
                    }
                });
                loom_rt_collect();
                collected.store(true, Ordering::Release);
                completion.notify();
                let _ = done.send(());
                parked(|| watchdog.join().unwrap());
            });
        });
        unsafe { crate::tasks::loom_rt_task_run(collection_task) };
        parked(|| worker.join().unwrap());
        assert!(RESUMED_AFTER_COLLECTION.get());
        COLLECTED.with(|value| value.borrow_mut().take());
    });
    loom_rt_collect();
}
