use super::tasks::*;
use super::{HEAP, loom_rt_box_new, loom_rt_collect, loom_rt_text_new, rooted, text_bytes};
use std::cell::RefCell;
use std::mem::size_of;
use std::ptr;

thread_local! {
    static EVENTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

fn event(value: &'static str) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}

#[repr(C)]
struct Frame {
    result: *mut u8,
    state: u64,
    first: u64,
    second: u64,
    saved: *mut u8,
}

unsafe extern "C" fn trace_frame(pointer: *mut u8) {
    let frame = pointer.cast::<Frame>();
    // SAFETY: Concrete frames start zeroed and these are their two GC fields.
    unsafe {
        super::trace_pointer(ptr::addr_of_mut!((*frame).result).cast());
        super::trace_pointer(ptr::addr_of_mut!((*frame).saved).cast());
    }
}

unsafe fn task(resume: unsafe extern "C-unwind" fn(*mut u8) -> i64, label: &'static str) -> u64 {
    let frame = loom_rt_box_new(size_of::<Frame>(), Some(trace_frame));
    // SAFETY: box_new zero-initializes this exact frame shape, creation does not
    // collect, and each label is static native UTF-8 rather than moving Text.
    unsafe { loom_rt_task_create(frame, resume, label.as_ptr(), label.len()) }
}

unsafe extern "C-unwind" fn first(frame: *mut u8) -> i64 {
    rooted([frame], |slots| {
        event("first");
        loom_rt_collect();
        // SAFETY: Use the rewritten frame after every allocating operation.
        unsafe {
            let value = loom_rt_text_new(b"first result".as_ptr(), 12);
            (*(*slots).cast::<Frame>()).result = value;
        }
        0
    })
}

unsafe extern "C-unwind" fn second(frame: *mut u8) -> i64 {
    rooted([frame], |slots| {
        event("second");
        loom_rt_collect();
        unsafe {
            let value = loom_rt_text_new(b"second result".as_ptr(), 13);
            (*(*slots).cast::<Frame>()).result = value;
        }
        0
    })
}

unsafe extern "C-unwind" fn parent_resume(frame: *mut u8) -> i64 {
    rooted([frame], |slots| {
        // SAFETY: All frame accesses reload from the authoritative slot. Task
        // operations do not collect; snapshot fields become rooted before release.
        unsafe {
            if (*(*slots).cast::<Frame>()).state == 0 {
                event("parent");
                let first = task(first, "first.loom:1:1: task created here");
                (*(*slots).cast::<Frame>()).first = first;
                let second = task(second, "second.loom:1:1: task created here");
                (*(*slots).cast::<Frame>()).second = second;
                (*(*slots).cast::<Frame>()).state = 1;
                event("created");
            }
            let first = (*(*slots).cast::<Frame>()).first;
            if loom_rt_task_await(first) == 0 {
                // Repeating this pending await must not add a second waiter.
                assert_eq!(loom_rt_task_await(first), 0);
                return 1;
            }
            let result = loom_rt_task_result(first).cast::<Frame>();
            (*(*slots).cast::<Frame>()).saved = (*result).result;
            loom_rt_task_release(first);
            loom_rt_collect();
            assert_eq!(
                text_bytes((*(*slots).cast::<Frame>()).saved),
                b"first result"
            );
            event("received first");
            let second = (*(*slots).cast::<Frame>()).second;
            assert_eq!(loom_rt_task_await(second), 1);
            let result = loom_rt_task_result(second).cast::<Frame>();
            assert_eq!(text_bytes((*result).result), b"second result");
            loom_rt_task_release(second);
            loom_rt_collect();
            assert_eq!(
                text_bytes((*(*slots).cast::<Frame>()).saved),
                b"first result"
            );
            event("received second");
        }
        0
    })
}

unsafe extern "C-unwind" fn construct() -> u64 {
    unsafe { task(parent_resume, "entry.loom:1:1: task created here") }
}

#[test]
fn hot_tasks_suspend_reload_and_root_results_before_release() {
    EVENTS.with(|events| events.borrow_mut().clear());
    loom_rt_collect();
    let baseline = HEAP.with(|heap| heap.borrow().objects.len());
    // SAFETY: The fixtures implement the typed resume/constructor ABI and keep
    // no lexical cleanup live across Pending; the owner cannot escape this call.
    unsafe { loom_rt_task_run(construct) };
    EVENTS.with(|events| {
        assert_eq!(
            *events.borrow(),
            [
                "parent",
                "created",
                "first",
                "second",
                "received first",
                "received second"
            ]
        );
    });
    loom_rt_collect();
    assert_eq!(HEAP.with(|heap| heap.borrow().objects.len()), baseline);
}

unsafe extern "C-unwind" fn never_run(_: *mut u8) -> i64 {
    event("unexpected cancelled task");
    0
}

unsafe extern "C-unwind" fn waiting_tree(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        event("waiting tree");
        let child = task(never_run, "grandchild.loom:1:1: task created here");
        (*(*slots).cast::<Frame>()).first = child;
        assert_eq!(loom_rt_task_await(child), 0);
        1
    })
}

unsafe extern "C-unwind" fn failing(_: *mut u8) -> i64 {
    event("child fault");
    super::fault("first child failure");
}

unsafe extern "C-unwind" fn fault_parent(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            let _tree = task(waiting_tree, "tree.loom:1:1: task created here");
            let child = task(failing, "failure.loom:4:2: task created here");
            (*(*slots).cast::<Frame>()).first = child;
            let _sibling = task(never_run, "sibling.loom:1:1: task created here");
            (*(*slots).cast::<Frame>()).state = 1;
        }
        let child = (*(*slots).cast::<Frame>()).first;
        if loom_rt_task_await(child) == 0 {
            return 1;
        }
        event("parent observes fault");
        loom_rt_task_result(child);
        unreachable!();
    })
}

unsafe extern "C-unwind" fn construct_fault() -> u64 {
    unsafe { task(fault_parent, "root.loom:2:1: task created here") }
}

#[test]
fn awaited_fault_cancels_queued_and_waiting_descendants_before_owner_exit() {
    EVENTS.with(|events| events.borrow_mut().clear());
    loom_rt_collect();
    let baseline = HEAP.with(|heap| heap.borrow().objects.len());
    // SAFETY: This outer boundary observes task_run's final propagation only
    // after the scheduler has released its roots and restored owner TLS.
    let failure =
        unsafe { super::cleanup::catch_fault(|| loom_rt_task_run(construct_fault)) }.unwrap_err();
    let diagnostic = std::str::from_utf8(&failure.message).unwrap();
    assert!(diagnostic.starts_with("first child failure"));
    assert!(diagnostic.contains("failure.loom:4:2: task created here"));
    assert!(diagnostic.contains("root.loom:2:1: task created here"));
    EVENTS.with(|events| {
        assert_eq!(
            *events.borrow(),
            ["waiting tree", "child fault", "parent observes fault"]
        );
    });
    loom_rt_collect();
    assert_eq!(HEAP.with(|heap| heap.borrow().objects.len()), baseline);
    // A new owner can run normally after the previous fault was drained.
    unsafe { loom_rt_task_run(construct) };
    loom_rt_collect();
    assert_eq!(HEAP.with(|heap| heap.borrow().objects.len()), baseline);
}
