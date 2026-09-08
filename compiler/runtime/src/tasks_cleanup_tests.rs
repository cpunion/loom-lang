use super::*;
use crate::cleanup::Cleanup;
use crate::{HEAP, loom_rt_box_new, loom_rt_collect, loom_rt_text_new, rooted, text_bytes};
use std::mem::{MaybeUninit, size_of};

unsafe extern "C" {
    fn loom_rt_cleanup_push(record: *mut Cleanup, callback: CleanupCallback, captures: *mut u8);
}

#[derive(Clone, Copy, PartialEq)]
enum Case {
    Normal,
    DuplicatePop,
    CleanupFault,
}

thread_local! {
    static CASE: Cell<Case> = const { Cell::new(Case::Normal) };
    static EVENTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

#[repr(C)]
struct Frame {
    result: u64,
    state: u64,
    first: u64,
    second: u64,
    text: *mut u8,
}

unsafe extern "C" fn trace_frame(pointer: *mut u8) {
    unsafe { crate::trace_pointer(ptr::addr_of_mut!((*pointer.cast::<Frame>()).text).cast()) };
}

unsafe fn task(resume: Resume) -> u64 {
    let frame = loom_rt_box_new(size_of::<Frame>(), Some(trace_frame));
    unsafe { loom_rt_task_create(frame, resume, ptr::null(), 0) }
}

fn event(label: &'static str) {
    EVENTS.with(|events| events.borrow_mut().push(label));
}

unsafe extern "C-unwind" fn inner_cleanup(frame: *mut u8) {
    rooted([frame], |slots| unsafe {
        event("inner");
        let text = loom_rt_text_new(b"last".as_ptr(), 4);
        // Assignment is authoritative immediately, not a callback-tail writeback.
        (*(*slots).cast::<Frame>()).text = text;
        loom_rt_collect();
        if CASE.get() == Case::CleanupFault {
            fault("first cleanup failure");
        }
    });
}

unsafe extern "C-unwind" fn outer_cleanup(frame: *mut u8) {
    rooted([frame], |slots| unsafe {
        loom_rt_collect();
        assert_eq!(text_bytes((*(*slots).cast::<Frame>()).text), b"last");
        event("outer");
        if CASE.get() == Case::CleanupFault {
            fault("secondary cleanup failure");
        }
    });
}

unsafe extern "C-unwind" fn last_cleanup(frame: *mut u8) {
    rooted([frame], |slots| unsafe {
        loom_rt_collect();
        assert_eq!(text_bytes((*(*slots).cast::<Frame>()).text), b"last");
        event("last");
    });
}

unsafe extern "C-unwind" fn collector(_: *mut u8) -> i64 {
    if loom_rt_task_wait_timer(0) == 0 {
        return 1;
    }
    // The parent has no native activation, but its captured Text and frame
    // remain traced. There is no stack capture record to retain accidentally.
    loom_rt_collect();
    0
}

unsafe extern "C-unwind" fn suspended(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            let text = loom_rt_text_new(b"initial".as_ptr(), 7);
            (*(*slots).cast::<Frame>()).text = text;
            loom_rt_task_cleanup_push(0, last_cleanup);
            loom_rt_task_cleanup_push(1, outer_cleanup);
            loom_rt_task_cleanup_push(2, inner_cleanup);
            let child = task(collector);
            (*(*slots).cast::<Frame>()).first = child;
            (*(*slots).cast::<Frame>()).state = 1;
        }
        let child = (*(*slots).cast::<Frame>()).first;
        if loom_rt_task_await(child) == 0 {
            return 1;
        }
        loom_rt_task_release(child);
        assert_eq!(text_bytes((*(*slots).cast::<Frame>()).text), b"initial");
        loom_rt_task_cleanup_pop(2);
        inner_cleanup(*slots);
        if CASE.get() == Case::DuplicatePop {
            loom_rt_task_cleanup_pop(2);
        }
        loom_rt_task_cleanup_pop(1);
        outer_cleanup(*slots);
        loom_rt_task_cleanup_pop(0);
        last_cleanup(*slots);
        0
    })
}

unsafe extern "C-unwind" fn construct_suspended() -> u64 {
    unsafe { task(suspended) }
}

fn run(constructor: Constructor, expected: Option<&[u8]>, events: &[&str]) {
    EVENTS.with(|events| events.borrow_mut().clear());
    loom_rt_collect();
    let baseline = HEAP.with(|heap| heap.borrow().objects.len());
    let result = unsafe { catch_fault(|| loom_rt_task_run(constructor)) };
    if let Some(message) = expected {
        assert_eq!(result.unwrap_err().message, message);
    } else {
        assert!(result.is_ok());
    }
    assert!(OWNER.get().is_null());
    EVENTS.with(|actual| assert_eq!(*actual.borrow(), events));
    loom_rt_collect();
    assert_eq!(HEAP.with(|heap| heap.borrow().objects.len()), baseline);
}

#[test]
fn suspended_captures_survive_gc_and_normal_cleanup_is_lifo() {
    CASE.set(Case::Normal);
    run(construct_suspended, None, &["inner", "outer", "last"]);
}

#[test]
fn duplicate_pop_and_cleanup_faults_drain_once_and_preserve_first_diagnostic() {
    CASE.set(Case::DuplicatePop);
    run(
        construct_suspended,
        Some(b"invalid task cleanup registration order"),
        &["inner", "outer", "last"],
    );
    CASE.set(Case::CleanupFault);
    run(
        construct_suspended,
        Some(b"first cleanup failure"),
        &["inner", "outer", "last"],
    );
}

unsafe extern "C-unwind" fn grandchild_cleanup(_: *mut u8) {
    event("grandchild");
    loom_rt_collect();
}

unsafe extern "C-unwind" fn child_inner(frame: *mut u8) {
    rooted([frame], |slots| unsafe {
        event("child inner");
        let text = loom_rt_text_new(b"updated".as_ptr(), 7);
        (*(*slots).cast::<Frame>()).text = text;
        fault("child cleanup failure");
    });
}

unsafe extern "C-unwind" fn child_outer(frame: *mut u8) {
    rooted([frame], |slots| unsafe {
        loom_rt_collect();
        assert_eq!(text_bytes((*(*slots).cast::<Frame>()).text), b"updated");
        event("child outer");
    });
}

unsafe extern "C-unwind" fn grandchild(_: *mut u8) -> i64 {
    loom_rt_task_cleanup_push(0, grandchild_cleanup);
    assert_eq!(loom_rt_task_wait_timer(i64::MAX), 0);
    1
}

unsafe extern "C-unwind" fn child(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        loom_rt_task_cleanup_push(0, child_outer);
        loom_rt_task_cleanup_push(1, child_inner);
        let grandchild = task(grandchild);
        (*(*slots).cast::<Frame>()).first = grandchild;
        assert_eq!(loom_rt_task_await(grandchild), 0);
        1
    })
}

fn assert_drained() {
    let owner = unsafe { &*OWNER.get() };
    let core = owner.core.borrow();
    assert_eq!(core.tasks.len(), 1);
    assert_eq!(core.pending, 0);
}

unsafe extern "C-unwind" fn helper_cleanup(captures: *mut u8) {
    // This is a live native helper capture: its address points to the parent's
    // authoritative stack root slot, not to a pre-GC frame snapshot.
    assert_drained();
    event("helper");
    loom_rt_collect();
    unsafe {
        let frame = *captures.cast::<*mut u8>();
        assert_eq!(text_bytes((*frame.cast::<Frame>()).text), b"parent");
    }
    fault("helper cleanup failure");
}

unsafe fn failing_helper(captures: *mut u8) -> ! {
    let mut record = MaybeUninit::<Cleanup>::uninit();
    unsafe { loom_rt_cleanup_push(record.as_mut_ptr(), helper_cleanup, captures) };
    fault("parent activation failure");
}

unsafe extern "C-unwind" fn parent_cleanup(frame: *mut u8) {
    rooted([frame], |slots| unsafe {
        assert_drained();
        loom_rt_collect();
        assert_eq!(text_bytes((*(*slots).cast::<Frame>()).text), b"parent");
        event("parent");
    });
}

unsafe extern "C-unwind" fn fault_parent(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            let text = loom_rt_text_new(b"parent".as_ptr(), 6);
            (*(*slots).cast::<Frame>()).text = text;
            loom_rt_task_cleanup_push(0, parent_cleanup);
            let _child = task(child);
            let barrier = task(collector);
            (*(*slots).cast::<Frame>()).first = barrier;
            (*(*slots).cast::<Frame>()).state = 1;
        }
        let barrier = (*(*slots).cast::<Frame>()).first;
        if loom_rt_task_await(barrier) == 0 {
            return 1;
        }
        loom_rt_task_release(barrier);
        // Own registration also must retire before a native cleanup can close
        // its source, even though this activation never returns Pending.
        assert_eq!(loom_rt_task_wait_timer(i64::MAX), 0);
        failing_helper(slots.cast());
    })
}

unsafe extern "C-unwind" fn construct_fault() -> u64 {
    unsafe { task(fault_parent) }
}

#[test]
fn descendants_then_live_helper_then_parent_cleanup_with_first_fault_preserved() {
    run(
        construct_fault,
        Some(b"parent activation failure"),
        &[
            "grandchild",
            "child inner",
            "child outer",
            "helper",
            "parent",
        ],
    );
}
