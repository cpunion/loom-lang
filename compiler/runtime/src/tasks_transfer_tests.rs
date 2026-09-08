use super::*;
use crate::wait::loom_rt_monotonic_ns;
use crate::{HEAP, loom_rt_box_new, loom_rt_collect, rooted};
use std::mem::size_of;

#[derive(Clone, Copy, PartialEq)]
enum Case {
    Success,
    ConsumerFault,
    ReleaseBeforeExtract,
    ExtractTwice,
    AdoptTwice,
    ProducerFault,
    ReturnedChildFault,
    RootReturn,
    DuplicateReturn,
}

thread_local! {
    static CASE: Cell<Case> = const { Cell::new(Case::Success) };
}

#[repr(C)]
struct Frame {
    result: u64,
    state: u64,
    first: u64,
    second: u64,
    deadline: i64,
}

unsafe fn task(resume: Resume, argument: u64) -> u64 {
    let frame = loom_rt_box_new(size_of::<Frame>(), None);
    // Only scalar fields: no payload tracer, but the allocation base still
    // moves and is retained by the scheduler's ordinary frame root set.
    unsafe {
        (*frame.cast::<Frame>()).first = argument;
        loom_rt_task_create(frame, resume, ptr::null(), 0)
    }
}

fn inspect<R>(run: impl FnOnce(&Core) -> R) -> R {
    assert!(!OWNER.get().is_null());
    let owner = unsafe { &*OWNER.get() };
    run(&owner.core.borrow())
}

unsafe extern "C-unwind" fn timer(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            (*(*slots).cast::<Frame>()).deadline = loom_rt_monotonic_ns() + 20_000_000;
            (*(*slots).cast::<Frame>()).state = 1;
        }
        if loom_rt_task_wait_timer((*(*slots).cast::<Frame>()).deadline) == 0 {
            return 1;
        }
        loom_rt_collect();
        (*(*slots).cast::<Frame>()).result = 42;
        0
    })
}

unsafe extern "C-unwind" fn inner(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if CASE.get() == Case::ReturnedChildFault {
            fault("returned child failed");
        }
        if (*(*slots).cast::<Frame>()).state == 0 {
            let timer = task(timer, 0);
            (*(*slots).cast::<Frame>()).first = timer;
            (*(*slots).cast::<Frame>()).state = 1;
        }
        let child = (*(*slots).cast::<Frame>()).first;
        if loom_rt_task_await(child) == 0 {
            return 1;
        }
        (*(*slots).cast::<Frame>()).result = (*loom_rt_task_result(child).cast::<Frame>()).result;
        loom_rt_task_release(child);
        0
    })
}

unsafe extern "C-unwind" fn producer(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        let child = task(inner, 0);
        (*(*slots).cast::<Frame>()).result = loom_rt_task_return(child);
        if CASE.get() == Case::DuplicateReturn {
            loom_rt_task_return(child);
        }
        if CASE.get() == Case::ProducerFault {
            fault("producer failed after marking result");
        }
        0
    })
}

unsafe extern "C-unwind" fn barrier(_: *mut u8) -> i64 {
    i64::from(loom_rt_task_wait_timer(0) == 0)
}

unsafe extern "C-unwind" fn consumer(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            let outer = (*(*slots).cast::<Frame>()).first;
            assert_eq!(loom_rt_task_await(outer), 1);
            match CASE.get() {
                Case::ConsumerFault => fault("consumer failed before extraction"),
                Case::ReleaseBeforeExtract => loom_rt_task_release(outer),
                _ => {}
            }
            let inner = (*loom_rt_task_result(outer).cast::<Frame>()).result;
            (*(*slots).cast::<Frame>()).first = inner;
            if CASE.get() == Case::ExtractTwice {
                loom_rt_task_result(outer);
            }
            inspect(|core| {
                assert_eq!(core.tasks[&inner].parent, core.current);
                assert!(core.tasks[&outer].children.is_empty());
                assert_eq!(core.tasks[&outer].returned, 1);
                assert!(!core.tasks[&inner].returned_to_parent);
            });
            loom_rt_task_release(outer);
            (*(*slots).cast::<Frame>()).state = 1;
            loom_rt_collect();
        }
        let inner = (*(*slots).cast::<Frame>()).first;
        if loom_rt_task_await(inner) == 0 {
            return 1;
        }
        (*(*slots).cast::<Frame>()).result = (*loom_rt_task_result(inner).cast::<Frame>()).result;
        loom_rt_task_release(inner);
        0
    })
}

unsafe extern "C-unwind" fn parent_resume(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            let outer = task(producer, 0);
            (*(*slots).cast::<Frame>()).first = outer;
            if CASE.get() == Case::RootReturn {
                loom_rt_task_return(outer);
            }
            let barrier = task(barrier, 0);
            (*(*slots).cast::<Frame>()).second = barrier;
            (*(*slots).cast::<Frame>()).state = 1;
        }
        if (*(*slots).cast::<Frame>()).state == 1 {
            let barrier = (*(*slots).cast::<Frame>()).second;
            if loom_rt_task_await(barrier) == 0 {
                return 1;
            }
            loom_rt_task_release(barrier);
            let outer = (*(*slots).cast::<Frame>()).first;
            inspect(|core| {
                let producer = &core.tasks[&outer];
                if matches!(CASE.get(), Case::ProducerFault | Case::DuplicateReturn) {
                    assert!(matches!(producer.state, State::Faulted(_)));
                    assert!(producer.children.is_empty());
                } else {
                    assert!(matches!(producer.state, State::Completed));
                    assert_eq!(producer.returned, 1);
                    let inner = *producer.children.first().unwrap();
                    assert!(core.tasks[&inner].returned_to_parent);
                    assert_eq!(core.tasks[&inner].parent, Some(outer));
                    if CASE.get() == Case::ReturnedChildFault {
                        // The outer completed normally; its child's failure
                        // belongs to the later inner await, not extraction.
                        assert!(matches!(core.tasks[&inner].state, State::Faulted(_)));
                        return;
                    }
                    // The returned task itself waits on a timer child. Moving
                    // the completed wrapper must move this whole live subtree.
                    let State::Waiting(timer) = core.tasks[&inner].state else {
                        panic!("returned task should be waiting on its timer child");
                    };
                    assert!(core.tasks[&timer].external.is_some());
                }
            });
            let callee = task(consumer, outer);
            loom_rt_task_adopt(callee, outer);
            if CASE.get() == Case::AdoptTwice {
                loom_rt_task_adopt(callee, outer);
            }
            (*(*slots).cast::<Frame>()).first = callee;
            (*(*slots).cast::<Frame>()).state = 2;
            loom_rt_collect();
        }
        let callee = (*(*slots).cast::<Frame>()).first;
        if loom_rt_task_await(callee) == 0 {
            return 1;
        }
        assert_eq!((*loom_rt_task_result(callee).cast::<Frame>()).result, 42);
        loom_rt_task_release(callee);
        0
    })
}

unsafe extern "C-unwind" fn construct() -> u64 {
    unsafe { task(parent_resume, 0) }
}

fn run(case: Case, expected: Option<&[u8]>) {
    CASE.set(case);
    loom_rt_collect();
    let baseline = HEAP.with(|heap| heap.borrow().objects.len());
    // Faults leave the scheduler only after the entire transferred tree and
    // its timer registrations have drained, with owner/root TLS restored.
    let result = unsafe { catch_fault(|| loom_rt_task_run(construct)) };
    if let Some(message) = expected {
        assert_eq!(result.unwrap_err().message, message);
    } else {
        assert!(result.is_ok());
    }
    assert!(OWNER.get().is_null());
    loom_rt_collect();
    assert_eq!(HEAP.with(|heap| heap.borrow().objects.len()), baseline);
}

#[test]
fn completed_outer_adoption_preserves_returned_waiting_subtree() {
    run(Case::Success, None);
}

#[test]
fn result_extraction_is_linear_and_cancellation_keeps_the_whole_tree() {
    run(
        Case::ConsumerFault,
        Some(b"consumer failed before extraction"),
    );
    run(
        Case::ReleaseBeforeExtract,
        Some(b"task release requires extracting its Task result"),
    );
    run(Case::ExtractTwice, Some(b"task result already extracted"));
}

#[test]
fn partial_adoption_and_return_faults_drain_without_escaping_tasks() {
    run(Case::AdoptTwice, Some(b"task belongs to another parent"));
    run(
        Case::ProducerFault,
        Some(b"producer failed after marking result"),
    );
    run(Case::ReturnedChildFault, Some(b"returned child failed"));
    run(Case::RootReturn, Some(b"async entry cannot return a Task"));
    run(
        Case::DuplicateReturn,
        Some(b"task return requires a distinct unawaited child"),
    );
}
