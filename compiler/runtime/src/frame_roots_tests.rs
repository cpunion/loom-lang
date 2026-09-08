use super::frame_roots::{FrameRootId, FrameRoots, with_frame_roots};
use super::{
    HEAP, loom_rt_box_new, loom_rt_collect, loom_rt_text_new, rooted, text_bytes, trace_pointer,
};
use std::mem::size_of;
use std::ptr;

#[repr(C)]
struct Frame {
    serial: u64,
    text: *mut u8,
    alias: *mut u8,
    next: *mut u8,
}

unsafe extern "C" fn trace_frame(pointer: *mut u8) {
    let frame = pointer.cast::<Frame>();
    // SAFETY: The zeroed concrete frame always contains initialized pointer
    // slots. Visiting only rewrites references; no allocation occurs here.
    unsafe {
        trace_pointer(ptr::addr_of_mut!((*frame).text).cast());
        trace_pointer(ptr::addr_of_mut!((*frame).alias).cast());
        trace_pointer(ptr::addr_of_mut!((*frame).next).cast());
    }
}

#[inline(never)]
fn create_frame(roots: &FrameRoots, label: &str, serial: u64) -> FrameRootId {
    rooted([ptr::null_mut(); 2], |slots| {
        // SAFETY: Stack slots protect construction across allocating calls;
        // publish the initialized frame before this creator activation exits.
        unsafe {
            *slots = loom_rt_text_new(label.as_ptr(), label.len());
            *slots.add(1) = loom_rt_box_new(size_of::<Frame>(), Some(trace_frame));
            (*slots.add(1)).cast::<Frame>().write(Frame {
                serial,
                text: *slots,
                alias: *slots,
                next: *slots.add(1),
            });
            roots.insert(*slots.add(1))
        }
    })
}

fn check_frame(roots: &FrameRoots, id: FrameRootId, label: &str, serial: u64) {
    let pointer = roots.get(id).unwrap();
    // SAFETY: Lookup reloads the current rooted pointer; assertions do not
    // allocate Loom objects or retain a Rust reference across collection.
    unsafe {
        let frame = &*pointer.cast::<Frame>();
        assert_eq!(frame.serial, serial);
        assert_eq!(frame.text, frame.alias);
        assert_eq!(frame.next, pointer);
        assert_eq!(text_bytes(frame.text), label.as_bytes());
    }
}

fn live() -> usize {
    HEAP.with(|heap| heap.borrow().objects.len())
}

#[test]
fn suspended_typed_frame_moves_after_creator_returns() {
    loom_rt_collect();
    assert_eq!(live(), 0);
    with_frame_roots(|roots| {
        // A frame may collect while its pointer fields are still zeroed, before
        // the creator fills its captures or state. Its concrete tracer is valid.
        let empty = unsafe { roots.insert(loom_rt_box_new(size_of::<Frame>(), Some(trace_frame))) };
        loom_rt_collect();
        let pointer = roots.get(empty).unwrap().cast::<Frame>();
        unsafe {
            assert_eq!((*pointer).serial, 0);
            assert!((*pointer).text.is_null());
            assert!((*pointer).alias.is_null());
            assert!((*pointer).next.is_null());
        }
        assert!(roots.remove(empty));
        loom_rt_collect();
        let id = create_frame(roots, "shared result 雪", 7);
        let before = roots.get(id).unwrap() as usize;
        loom_rt_collect();
        assert_ne!(roots.get(id).unwrap() as usize, before);
        check_frame(roots, id, "shared result 雪", 7);
        assert_eq!(live(), 2);
        let relocated = roots.get(id).unwrap() as usize;
        loom_rt_collect();
        assert_ne!(roots.get(id).unwrap() as usize, relocated);
        check_frame(roots, id, "shared result 雪", 7);
        // Leaving the owner scope releases its still-registered frame root.
    });
    loom_rt_collect();
    assert_eq!(live(), 0);
}

#[test]
fn frame_roots_grow_remove_out_of_order_and_reject_stale_ids() {
    loom_rt_collect();
    assert_eq!(live(), 0);
    with_frame_roots(|roots| {
        const COUNT: usize = 128;
        let mut ids = Vec::new();
        for index in 0..COUNT {
            ids.push(create_frame(roots, "frame", index as u64));
            if index % 16 == 15 {
                // Collection repeatedly crosses root storage growth. Its
                // registration must not point into a former Vec allocation.
                loom_rt_collect();
            }
        }
        let removed = [1, COUNT / 2, COUNT - 1];
        for index in removed {
            assert!(roots.remove(ids[index]));
            assert!(!roots.remove(ids[index]));
            assert!(roots.get(ids[index]).is_none());
        }
        loom_rt_collect();
        assert_eq!(live(), (COUNT - removed.len()) * 2);
        let replacement = create_frame(roots, "replacement", 999);
        let stale = ids[COUNT - 1];
        assert_eq!(replacement.key, stale.key);
        assert_ne!(replacement.generation, stale.generation);
        let replacement_pointer = roots.get(replacement).unwrap();
        // SAFETY: Replacement is a current managed object; these root-table
        // operations do not collect. A stale ID must not overwrite its reuse.
        unsafe {
            assert!(!roots.replace(stale, replacement_pointer));
            assert!(roots.replace(ids[2], replacement_pointer));
        }
        assert!(!roots.remove(stale));
        assert!(roots.remove(replacement));
        loom_rt_collect();
        assert_eq!(live(), (COUNT - removed.len()) * 2);
        for (index, &id) in ids.iter().enumerate() {
            if removed.contains(&index) {
                assert!(roots.get(id).is_none());
            } else {
                if index == 2 {
                    check_frame(roots, id, "replacement", 999);
                } else {
                    check_frame(roots, id, "frame", index as u64);
                }
                assert!(roots.remove(id));
            }
        }
        loom_rt_collect();
        assert_eq!(live(), 0);
    });
    loom_rt_collect();
    assert_eq!(live(), 0);
}

#[test]
fn completion_identity_reloads_frame_and_hands_result_to_stack_roots() {
    use super::wait::{COMPLETION, KIND_COMPLETION, Reactor, WaitSource};
    use std::sync::Arc;

    loom_rt_collect();
    assert_eq!(live(), 0);
    with_frame_roots(|roots| {
        let child = create_frame(roots, "finished", 33);
        let before = roots.get(child).unwrap() as usize;
        let reactor = Arc::new(Reactor::new().unwrap());
        let owner = 33;
        // SAFETY: A completion registration borrows no descriptor or managed
        // pointer. Only this owner's integer identity crosses the thread.
        let registration = unsafe {
            reactor.register(
                WaitSource {
                    kind: KIND_COMPLETION,
                    interests: 0,
                    handle: 0,
                    deadline_ns: 0,
                },
                owner,
            )
        }
        .unwrap();
        let producer = {
            let reactor = Arc::clone(&reactor);
            std::thread::spawn(move || {
                reactor
                    .notify_completion(registration, COMPLETION, 0)
                    .unwrap()
            })
        };
        loom_rt_collect();
        assert!(producer.join().unwrap());
        let notification = reactor.pop_ready().unwrap();
        assert_eq!(notification.owner, owner);
        // The owner retains the complete generation-checked frame ID. A raw
        // reusable slot key alone is not a future scheduler's Task identity.
        let pointer = roots.get(child).unwrap();
        assert_ne!(pointer as usize, before);
        check_frame(roots, child, "finished", 33);
        // SAFETY: Read the result while the child is still rooted. rooted()
        // registers its stack slots without collecting; only then retire the
        // child. There is no allocation/collection in an unrooted handoff gap.
        let result = unsafe { (*pointer.cast::<Frame>()).text };
        rooted([result, result], |slots| {
            assert!(roots.remove(child));
            loom_rt_collect();
            assert_eq!(live(), 1); // The child/self-cycle is gone; Text remains.
            unsafe {
                assert_eq!(*slots, *slots.add(1));
                assert_eq!(text_bytes(*slots), b"finished");
            }
        });
        loom_rt_collect();
        assert_eq!(live(), 0);
    });
    loom_rt_collect();
    assert_eq!(live(), 0);
}
