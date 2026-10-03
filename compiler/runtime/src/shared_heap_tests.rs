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
    use crate::native::{WorkerControl, WorkerExit};
    unsafe extern "C-unwind" {
        fn loom_rt_worker_checkpoint();
    }
    let input = unsafe { loom_rt_text_new(b"checkpoint".as_ptr(), 10) };
    with_handoff(input, |handoff| {
        scope(|heap| {
            let heap = heap as usize;
            let handoff = Arc::clone(&handoff);
            let ready = Arc::new(AtomicBool::new(false));
            let control = Arc::new(WorkerControl::default());
            let polls = Arc::new(AtomicUsize::new(0));
            let worker = {
                let ready = Arc::clone(&ready);
                let control = Arc::clone(&control);
                let polls = Arc::clone(&polls);
                thread::spawn(move || unsafe {
                    attach(heap, || {
                        let result: Result<(), _> = control.run(|| {
                            rooted([handoff.input.load(Ordering::Acquire)], |slots| {
                                ready.store(true, Ordering::Release);
                                loop {
                                    loom_rt_worker_checkpoint();
                                    assert_eq!(*slots, handoff.input.load(Ordering::Acquire));
                                    assert_eq!(text_bytes(*slots), b"checkpoint");
                                    polls.fetch_add(1, Ordering::Relaxed);
                                    thread::yield_now();
                                }
                            });
                        });
                        assert!(matches!(result, Err(WorkerExit::Cancelled)));
                    });
                })
            };
            while !ready.load(Ordering::Acquire) {
                thread::yield_now();
            }
            for _ in 0..16 {
                loom_rt_collect();
            }
            control.cancel();
            parked(|| worker.join().unwrap());
            assert!(polls.load(Ordering::Relaxed) > 0);
        });
    });
    loom_rt_collect();
}

#[cfg(unix)]
#[test]
fn blocking_file_read_parks_without_retaining_a_managed_interior() {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    scope(|heap| {
        let heap = heap as usize;
        let worker = thread::spawn(move || unsafe {
            attach(heap, || {
                rooted([crate::loom_rt_bytes_new()], |slots| {
                    assert_eq!(crate::loom_rt_file_read(i64::from(pipe[0]), *slots, 4), 4);
                    assert_eq!(crate::buffer_bytes(*slots), b"read");
                    assert_eq!(crate::loom_rt_file_close(i64::from(pipe[0])), 0);
                });
            });
        });
        // Wait for the file boundary itself to publish the worker's roots.
        loop {
            let parked = lock(&unsafe { &*(heap as *const SharedHeap) }.control)
                .participants
                .values()
                .any(Option::is_some);
            if parked {
                break;
            }
            thread::yield_now();
        }
        loom_rt_collect();
        assert_eq!(
            unsafe { libc::write(pipe[1], b"read".as_ptr().cast(), 4) },
            4
        );
        assert_eq!(unsafe { libc::close(pipe[1]) }, 0);
        parked(|| worker.join().unwrap());
    });
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

#[test]
fn scoped_mutex_primitives_serialize_updates_while_waiters_participate_in_gc() {
    use crate::mutex::{loom_rt_mutex_lock, loom_rt_mutex_unlock};
    with_handoff(crate::loom_rt_box_new(8, None), |handoff| {
        scope(|heap| {
            handoff.outputs[0].store(crate::loom_rt_box_new(8, None), Ordering::Release);
            let mut workers = Vec::new();
            for _ in 0..2 {
                let heap = heap as usize;
                let handoff = Arc::clone(&handoff);
                workers.push(thread::spawn(move || unsafe {
                    attach(heap, || {
                        rooted(
                            [
                                handoff.input.load(Ordering::Acquire),
                                handoff.outputs[0].load(Ordering::Acquire),
                            ],
                            |slots| {
                                for _ in 0..16 {
                                    let guard = loom_rt_mutex_lock(*slots.add(1));
                                    let value = (*slots).cast::<u64>().read();
                                    loom_rt_collect();
                                    (*slots).cast::<u64>().write(value + 1);
                                    assert_eq!(loom_rt_mutex_unlock(guard), 1);
                                }
                            },
                        );
                    });
                }));
            }
            parked(|| {
                for worker in workers {
                    worker.join().unwrap();
                }
            });
            assert_eq!(
                unsafe { handoff.input.load(Ordering::Acquire).cast::<u64>().read() },
                32
            );
        });
    });
    loom_rt_collect();
    assert!(HEAP.with(|heap| heap.borrow().mutexes.is_empty()));
}

#[test]
fn cancelled_mutex_waiter_drains_with_live_roots_before_owner_releases_lock() {
    use crate::mutex::{loom_rt_mutex_lock, loom_rt_mutex_unlock};
    use crate::native::{WorkerControl, WorkerExit};

    unsafe extern "C" {
        fn loom_rt_cleanup_push(
            record: *mut crate::cleanup::Cleanup,
            callback: unsafe extern "C-unwind" fn(*mut u8),
            captures: *mut u8,
        );
    }
    unsafe extern "C-unwind" {
        fn loom_rt_worker_checkpoint();
    }

    struct Captures<'a> {
        slots: *mut *mut u8,
        cleaned: &'a mut bool,
    }

    unsafe extern "C-unwind" fn clean(data: *mut u8) {
        let captures = unsafe { &mut *data.cast::<Captures<'_>>() };
        loom_rt_collect();
        unsafe {
            assert_eq!(text_bytes(*captures.slots), b"still rooted");
            // Mandatory cleanup ignores the request and may use a different
            // mutex. GC above also proves cancellation resumed the mutator.
            loom_rt_worker_checkpoint();
            let key = crate::loom_rt_box_new(8, None);
            let guard = loom_rt_mutex_lock(key);
            assert_eq!(loom_rt_mutex_unlock(guard), 1);
        }
        *captures.cleaned = true;
    }

    with_handoff(crate::loom_rt_box_new(8, None), |handoff| {
        scope(|heap| {
            let guard = unsafe { loom_rt_mutex_lock(handoff.input.load(Ordering::Acquire)) };
            let control = Arc::new(WorkerControl::default());
            let child_control = Arc::clone(&control);
            let child_handoff = Arc::clone(&handoff);
            let heap = heap as usize;
            let (send, receive) = std::sync::mpsc::channel();
            let worker = thread::spawn(move || unsafe {
                attach(heap, || {
                    let mut cleaned = false;
                    let result = child_control.run(|| {
                        let text = loom_rt_text_new(b"still rooted".as_ptr(), 12);
                        rooted([text], |slots| {
                            let mut captures = Captures {
                                slots,
                                cleaned: &mut cleaned,
                            };
                            let mut record = MaybeUninit::uninit();
                            loom_rt_cleanup_push(
                                record.as_mut_ptr(),
                                clean,
                                ptr::from_mut(&mut captures).cast(),
                            );
                            let acquired =
                                loom_rt_mutex_lock(child_handoff.input.load(Ordering::Acquire));
                            // Only reached by the timeout recovery of a broken
                            // cancellation implementation; balance before exit.
                            loom_rt_mutex_unlock(acquired);
                            crate::cleanup::fault(b"cancelled acquisition returned");
                        });
                    });
                    send.send((result, cleaned)).unwrap();
                });
            });
            // Registration publishes no borrowed managed address. Collection
            // waits for the child to park, and relocates the shared lock key.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !control.waiting() && std::time::Instant::now() < deadline {
                loom_rt_shared_checkpoint();
                thread::yield_now();
            }
            let waiting = control.waiting();
            if waiting {
                loom_rt_collect();
            }
            control.cancel();
            let result = park_native(|| receive.recv_timeout(std::time::Duration::from_secs(10)));
            // The successful path has already drained the child without this
            // release. On regression, release before joining to report failure.
            assert_eq!(loom_rt_mutex_unlock(guard), 1);
            parked(|| worker.join().unwrap());
            assert!(waiting, "worker never registered its blocked acquisition");
            let (result, cleaned) = result.expect("cancelled child did not drain while lock held");
            assert!(matches!(result, Err(WorkerExit::Cancelled)), "{result:?}");
            assert!(cleaned);
            let next = unsafe { loom_rt_mutex_lock(handoff.input.load(Ordering::Acquire)) };
            assert_eq!(loom_rt_mutex_unlock(next), 1);
        });
    });
    loom_rt_collect();
}
