use super::*;
use crate::wait::loom_rt_monotonic_ns;
use crate::{HEAP, loom_rt_box_new, loom_rt_collect, loom_rt_text_new, rooted, text_bytes};
use std::mem::size_of;

thread_local! {
    static EVENTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

#[repr(C)]
struct Frame {
    result: *mut u8,
    state: u64,
    first: u64,
    second: u64,
    deadline: i64,
    saved: *mut u8,
}

unsafe extern "C" fn trace_frame(pointer: *mut u8) {
    let frame = pointer.cast::<Frame>();
    unsafe {
        crate::trace_pointer(ptr::addr_of_mut!((*frame).result).cast());
        crate::trace_pointer(ptr::addr_of_mut!((*frame).saved).cast());
    }
}

unsafe fn task(resume: Resume) -> u64 {
    let frame = loom_rt_box_new(size_of::<Frame>(), Some(trace_frame));
    unsafe { loom_rt_task_create(frame, resume, ptr::null(), 0) }
}

fn with_owner<R>(run: impl FnOnce(&Owner) -> R) -> R {
    // The reference cannot escape the enclosing synchronous fixture callback.
    assert!(!OWNER.get().is_null());
    run(unsafe { &*OWNER.get() })
}

fn event(label: &'static str) {
    EVENTS.with(|events| events.borrow_mut().push(label));
}

unsafe fn timer(frame: *mut u8, label: &'static str) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            event(label);
            // Move both this frame and any already-suspended sibling frame.
            loom_rt_collect();
            let text = loom_rt_text_new(label.as_ptr(), label.len());
            (*(*slots).cast::<Frame>()).saved = text;
            (*(*slots).cast::<Frame>()).deadline = loom_rt_monotonic_ns() + 20_000_000;
            (*(*slots).cast::<Frame>()).state = 1;
        }
        let deadline = (*(*slots).cast::<Frame>()).deadline;
        if loom_rt_task_wait_timer(deadline) == 0 {
            assert_eq!(loom_rt_task_wait_timer(deadline), 0);
            return 1;
        }
        assert!(loom_rt_monotonic_ns() >= deadline);
        loom_rt_collect();
        assert_eq!(
            text_bytes((*(*slots).cast::<Frame>()).saved),
            label.as_bytes()
        );
        (*(*slots).cast::<Frame>()).result = (*(*slots).cast::<Frame>()).saved;
        0
    })
}

unsafe extern "C-unwind" fn first(frame: *mut u8) -> i64 {
    unsafe { timer(frame, "first") }
}

unsafe extern "C-unwind" fn second(frame: *mut u8) -> i64 {
    unsafe { timer(frame, "second") }
}

unsafe extern "C-unwind" fn parent(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            with_owner(|owner| assert!(owner.reactor.get().is_none()));
            let first = task(first);
            (*(*slots).cast::<Frame>()).first = first;
            let second = task(second);
            (*(*slots).cast::<Frame>()).second = second;
            (*(*slots).cast::<Frame>()).state = 1;
        }
        if (*(*slots).cast::<Frame>()).state == 1 {
            let first = (*(*slots).cast::<Frame>()).first;
            if loom_rt_task_await(first) == 0 {
                return 1;
            }
            (*(*slots).cast::<Frame>()).saved =
                (*loom_rt_task_result(first).cast::<Frame>()).result;
            loom_rt_task_release(first);
            (*(*slots).cast::<Frame>()).state = 2;
            loom_rt_collect();
            assert_eq!(text_bytes((*(*slots).cast::<Frame>()).saved), b"first");
        }
        let second = (*(*slots).cast::<Frame>()).second;
        if loom_rt_task_await(second) == 0 {
            return 1;
        }
        assert_eq!(
            text_bytes((*loom_rt_task_result(second).cast::<Frame>()).result),
            b"second"
        );
        loom_rt_task_release(second);
        loom_rt_collect();
        assert_eq!(text_bytes((*(*slots).cast::<Frame>()).saved), b"first");
        event("joined");
        0
    })
}

unsafe extern "C-unwind" fn construct() -> u64 {
    unsafe { task(parent) }
}

#[test]
fn timers_suspend_hot_siblings_and_reload_moving_frames() {
    EVENTS.with(|events| events.borrow_mut().clear());
    loom_rt_collect();
    let baseline = HEAP.with(|heap| heap.borrow().objects.len());
    unsafe { loom_rt_task_run(construct) };
    EVENTS.with(|events| assert_eq!(*events.borrow(), ["first", "second", "joined"]));
    loom_rt_collect();
    assert_eq!(HEAP.with(|heap| heap.borrow().objects.len()), baseline);
}

fn registration() -> Registration {
    with_owner(|owner| {
        let core = owner.core.borrow();
        core.tasks[&core.current.unwrap()]
            .external
            .unwrap()
            .registration
    })
}

fn cancel_current() {
    with_owner(|owner| {
        let mut core = owner.core.borrow_mut();
        let id = core.current.unwrap();
        owner.cancel_wait(&mut core, id);
        core.tasks.get_mut(&id).unwrap().state = State::Running;
    });
}

unsafe extern "C-unwind" fn stale(_: *mut u8) -> i64 {
    with_owner(|owner| {
        assert_eq!(loom_rt_task_wait_timer(i64::MAX), 0);
        let cancelled = registration();
        cancel_current();
        assert!(!owner.reactor.get().unwrap().cancel(cancelled).unwrap());

        assert_eq!(loom_rt_task_wait_timer(0), 0);
        let fired = registration();
        assert_eq!(cancelled.key, fired.key);
        assert_ne!(cancelled.generation, fired.generation);
        owner
            .reactor
            .get()
            .unwrap()
            .wait(Some(Duration::ZERO))
            .unwrap();
        // This cancellation sees an already-fired registration, leaving its old
        // notification queued while a replacement reuses the registration slot.
        cancel_current();
        assert_eq!(loom_rt_task_wait_timer(i64::MAX), 0);
        let replacement = registration();
        assert_eq!(fired.key, replacement.key);
        assert_ne!(fired.generation, replacement.generation);
        owner.poll(false).unwrap();
        {
            let core = owner.core.borrow();
            let task = &core.tasks[&core.current.unwrap()];
            assert!(matches!(task.state, State::ExternalWaiting));
            assert!(task.external.unwrap().ready.is_none());
            assert_eq!(core.pending, 1);
            assert!(core.ready.is_empty());
        }
        cancel_current();
        assert_eq!(owner.core.borrow().pending, 0);
        0
    })
}

unsafe extern "C-unwind" fn construct_stale() -> u64 {
    unsafe { task(stale) }
}

#[test]
fn cancelled_and_fired_registrations_cannot_wake_a_reused_slot() {
    unsafe { loom_rt_task_run(construct_stale) };
}

unsafe extern "C-unwind" fn pending_timer(_: *mut u8) -> i64 {
    assert_eq!(loom_rt_task_wait_timer(i64::MAX), 0);
    event("waiting sibling");
    1
}

unsafe extern "C-unwind" fn fault_after_register(_: *mut u8) -> i64 {
    assert_eq!(loom_rt_task_wait_timer(i64::MAX), 0);
    event("fault after registration");
    fault("timer activation failed");
}

unsafe extern "C-unwind" fn fault_parent(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            let _sibling = task(pending_timer);
            let failing = task(fault_after_register);
            (*(*slots).cast::<Frame>()).first = failing;
            (*(*slots).cast::<Frame>()).state = 1;
        }
        let failing = (*(*slots).cast::<Frame>()).first;
        if loom_rt_task_await(failing) == 0 {
            return 1;
        }
        // Only the sibling remains registered: the failing activation retired
        // its own wait even though it never returned Pending.
        with_owner(|owner| assert_eq!(owner.core.borrow().pending, 1));
        loom_rt_task_result(failing);
        unreachable!();
    })
}

unsafe extern "C-unwind" fn construct_fault() -> u64 {
    unsafe { task(fault_parent) }
}

unsafe extern "C-unwind" fn negative(_: *mut u8) -> i64 {
    with_owner(|owner| assert!(owner.reactor.get().is_none()));
    loom_rt_task_wait_timer(-1);
    unreachable!();
}

unsafe extern "C-unwind" fn construct_negative() -> u64 {
    unsafe { task(negative) }
}

unsafe extern "C-unwind" fn cpu(_: *mut u8) -> i64 {
    with_owner(|owner| assert!(owner.reactor.get().is_none()));
    0
}

unsafe extern "C-unwind" fn construct_cpu() -> u64 {
    unsafe { task(cpu) }
}

#[test]
fn faults_cancel_own_and_descendant_timers_before_owner_exit() {
    EVENTS.with(|events| events.borrow_mut().clear());
    loom_rt_collect();
    let baseline = HEAP.with(|heap| heap.borrow().objects.len());
    let failure = unsafe { catch_fault(|| loom_rt_task_run(construct_fault)) }.unwrap_err();
    assert_eq!(failure.message, b"timer activation failed");
    EVENTS.with(|events| {
        assert_eq!(
            *events.borrow(),
            ["waiting sibling", "fault after registration"]
        );
    });
    assert!(OWNER.get().is_null());
    let failure = unsafe { catch_fault(|| loom_rt_task_run(construct_negative)) }.unwrap_err();
    assert_eq!(failure.message, b"timer deadline must not be negative");
    unsafe { loom_rt_task_run(construct_cpu) };
    loom_rt_collect();
    assert_eq!(HEAP.with(|heap| heap.borrow().objects.len()), baseline);
}
