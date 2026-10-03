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
                        rooted([child_handoff.input.load(Ordering::Acquire)], |_| {
                            loom_rt_collect();
                            crate::fault("worker failure");
                        });
                    })
                    .unwrap_err();
                    assert_eq!(failure.message, b"worker failure");
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
