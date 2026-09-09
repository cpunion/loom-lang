//! Register each child once, then select terminal notifications without polling
//! a source container. Only IDs/indices are retained; managed values stay rooted
//! in their typed frames. Selection is not result extraction or join policy.

use super::*;

#[derive(Default)]
pub(super) struct Observation {
    members: HashMap<u64, i64>,
    ready: BTreeSet<(u64, u64)>,
}

pub(super) fn contains(core: &Core, parent: u64, child: u64) -> bool {
    core.tasks[&parent]
        .observation
        .as_ref()
        .is_some_and(|group| group.members.contains_key(&child))
}

// Called before a cancelled child's frame/identity is removed. Its parent may
// itself be cancelled; removing an observation must not queue that parent.
pub(super) fn remove(core: &mut Core, child: u64) {
    let task = &core.tasks[&child];
    let parent = task.parent;
    let order = task.state.terminal_order();
    if let Some(parent) = parent
        && let Some(group) = &mut core.tasks.get_mut(&parent).unwrap().observation
    {
        group.members.remove(&child);
        if let Some(order) = order {
            group.ready.remove(&(order, child));
        }
    }
}

// Return false for an ordinary single-child await. Other observed completions
// stay queued even while the parent executes or awaits a different child.
pub(super) fn completed(core: &mut Core, parent: u64, child: u64) -> bool {
    let faulted = matches!(core.tasks[&child].state, State::Faulted(_, _));
    let order = core.tasks[&child]
        .state
        .terminal_order()
        .expect("terminal child");
    let task = core.tasks.get_mut(&parent).expect("live observer");
    let Some(group) = task
        .observation
        .as_mut()
        .filter(|group| group.members.contains_key(&child))
    else {
        return false;
    };
    assert!(group.ready.insert((order, child)));
    if matches!(task.state, State::Observing) {
        task.state = State::Queued;
        // Like direct await, make an observed fault available before starting
        // more queued work. Notification selection still uses terminal order.
        if faulted {
            core.ready.push_front(parent);
        } else {
            core.ready.push_back(parent);
        }
    }
    true
}

/// Keep the handle's type in generated code: registration returns that same
/// one-shot handle. An observed child cannot be adopted or returned elsewhere.
#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_observe(child: u64, index: i64) -> u64 {
    edit(|_, core| {
        let parent = parent(core, child)?;
        if index < 0 || !matches!(core.tasks[&parent].state, State::Running) {
            return Err("task observation requires a running parent and nonnegative index");
        }
        let task = core.tasks.get_mut(&child).unwrap();
        if task.returned_to_parent || task.waiter.is_some() {
            return Err("task observation requires an unawaited child");
        }
        task.waiter = Some(parent);
        let order = task.state.terminal_order();
        let group = core
            .tasks
            .get_mut(&parent)
            .unwrap()
            .observation
            .get_or_insert_with(Box::default);
        assert!(group.members.insert(child, index).is_none());
        // Already-completed children retain completion order, not registration
        // or input order. Newly completed children use the same queue.
        if let Some(order) = order {
            assert!(group.ready.insert((order, child)));
        }
        Ok(child)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_wait_next() -> i32 {
    edit(|_, core| {
        let current = core.current.ok_or("task observation outside a resume")?;
        let task = core.tasks.get_mut(&current).unwrap();
        if !matches!(task.state, State::Running | State::Observing) || task.external.is_some() {
            return Err("task already has another wait");
        }
        let group = task.observation.as_ref().ok_or("no observed tasks")?;
        if group.members.is_empty() {
            return Err("no observed tasks");
        }
        let ready = !group.ready.is_empty();
        task.state = if ready {
            State::Running
        } else {
            State::Observing
        };
        Ok(i32::from(ready))
    })
}

/// Detach one notification. Its original typed handle still carries the
/// obligation to extract its result or cancel; no task/frame is released here.
#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_next_result() -> i64 {
    edit(|_, core| {
        let current = core.current.ok_or("task observation outside a resume")?;
        let task = core.tasks.get_mut(&current).unwrap();
        if !matches!(task.state, State::Running) {
            return Err("task observation result is not ready");
        }
        let group = task.observation.as_mut().ok_or("no observed tasks")?;
        let (_, child) = group
            .ready
            .pop_first()
            .ok_or("task observation result is not ready")?;
        let index = group.members.remove(&child).expect("registered completion");
        core.tasks.get_mut(&child).unwrap().waiter = None;
        Ok(index)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HEAP, loom_rt_box_new, loom_rt_collect, rooted};
    use std::mem::size_of;

    thread_local! {
        static EVENTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    #[repr(C)]
    struct Frame {
        result: i64,
        state: i64,
        failing: u64,
    }

    fn event(text: &'static str) {
        EVENTS.with(|events| events.borrow_mut().push(text));
    }

    unsafe fn task(resume: Resume) -> u64 {
        let frame = loom_rt_box_new(size_of::<Frame>(), None);
        unsafe { loom_rt_task_create(frame, resume, ptr::null(), 0) }
    }

    unsafe extern "C-unwind" fn child_cleanup(_: *mut u8) {
        event("child cleaned");
    }
    unsafe extern "C-unwind" fn parent_cleanup(_: *mut u8) {
        let owner = unsafe { &*OWNER.get() };
        let core = owner.core.borrow();
        let parent = &core.tasks[&core.root.unwrap()];
        assert!(parent.children.is_empty());
        let group = parent.observation.as_ref().unwrap();
        assert!(group.members.is_empty() && group.ready.is_empty());
        assert_eq!(core.pending, 0);
        event("parent cleaned");
    }

    unsafe extern "C-unwind" fn waiting(_: *mut u8) -> i64 {
        loom_rt_task_cleanup_push(0, child_cleanup);
        assert_eq!(loom_rt_task_wait_timer(i64::MAX), 0);
        event("child waiting");
        1
    }

    unsafe extern "C-unwind" fn failing(_: *mut u8) -> i64 {
        fault("observed failure");
    }

    unsafe extern "C-unwind" fn unstarted(_: *mut u8) -> i64 {
        event("unexpected start");
        0
    }

    unsafe extern "C-unwind" fn parent(frame: *mut u8) -> i64 {
        rooted([frame], |slots| unsafe {
            if (*(*slots).cast::<Frame>()).state == 0 {
                loom_rt_task_cleanup_push(0, parent_cleanup);
                let child = task(waiting);
                assert_eq!(loom_rt_task_observe(child, 0), child);
                let child = task(failing);
                (*(*slots).cast::<Frame>()).failing = loom_rt_task_observe(child, 1);
                loom_rt_task_observe(task(unstarted), 2);
                (*(*slots).cast::<Frame>()).state = 1;
            }
            if loom_rt_task_wait_next() == 0 {
                return 1;
            }
            // Moving collection does not affect notification identities.
            loom_rt_collect();
            assert_eq!(loom_rt_task_next_result(), 1);
            event("failure selected");
            let child = (*(*slots).cast::<Frame>()).failing;
            assert_eq!(loom_rt_task_await(child), 1);
            loom_rt_task_result(child);
            unreachable!()
        })
    }

    unsafe extern "C-unwind" fn construct() -> u64 {
        unsafe { task(parent) }
    }

    #[test]
    fn observed_fault_wakes_parent_and_cancellation_removes_the_other_observations() {
        EVENTS.with(|events| events.borrow_mut().clear());
        loom_rt_collect();
        let before = HEAP.with(|heap| heap.borrow().objects.len());
        let failure = unsafe { catch_fault(|| loom_rt_task_run(construct)) }.unwrap_err();
        assert!(String::from_utf8_lossy(&failure.message).contains("observed failure"));
        EVENTS.with(|events| {
            assert_eq!(
                *events.borrow(),
                [
                    "child waiting",
                    "failure selected",
                    "child cleaned",
                    "parent cleaned"
                ]
            )
        });
        loom_rt_collect();
        assert_eq!(HEAP.with(|heap| heap.borrow().objects.len()), before);
    }
}
