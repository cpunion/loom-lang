//! Terminal inspection copies fault data, never erases a typed result. Explicit
//! cancellation drains children, OS work and cleanup before reporting terminal.

use super::*;

fn awaited(core: &Core, child: u64) -> Result<&Task, &'static str> {
    let current = parent(core, child)?;
    let task = &core.tasks[&child];
    if task.waiter != Some(current) || task.state.terminal_order().is_none() {
        return Err("task outcome requires a terminal awaited child");
    }
    Ok(task)
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_status(child: u64) -> i64 {
    edit(|_, core| {
        Ok(match awaited(core, child)?.state {
            State::Completed(_) => 0,
            State::Faulted(_, _) => 1,
            State::Cancelled(_) => 2,
            _ => unreachable!(),
        })
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_failure(child: u64) -> *mut u8 {
    let bytes = edit(|_, core| {
        let State::Faulted(failure, _) = &awaited(core, child)?.state else {
            return Err("task fault data requires a faulted child");
        };
        let mut bytes = Vec::new();
        if let Some(name) = &failure.test_name {
            bytes.extend_from_slice(b"FAIL ");
            bytes.extend_from_slice(name);
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(&failure.message);
        Ok(bytes)
    });
    // No Core borrow or managed interior pointer crosses collection. The
    // caller roots/reloads its frame and any earlier expression snapshots.
    unsafe { crate::loom_rt_text_new(bytes.as_ptr(), bytes.len()) }
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_cancel_begin(child: u64) {
    let (owner, terminal) = edit(|owner, core| {
        let current = parent(core, child)?;
        let caller = &core.tasks[&current];
        let task = &core.tasks[&child];
        if !matches!(caller.state, State::Running)
            || caller.external.is_some()
            || task.returned_to_parent
            || task.waiter.is_some_and(|waiter| waiter != current)
        {
            return Err("task cancellation requires a running parent and its unconsumed child");
        }
        let terminal = task.state.terminal_order().is_some();
        observation::remove(core, child);
        core.tasks.get_mut(&child).unwrap().waiter = Some(current);
        Ok((ptr::from_ref(owner), terminal))
    });
    if terminal {
        // A completed value (including returned child Tasks) or existing fault
        // wins over cancellation. Generated code extracts that real outcome.
        return;
    }
    // SAFETY: The enclosing owner activation outlives this synchronous drain.
    let owner = unsafe { &*owner };
    let mut first = None;
    owner.cancel_descendants(child, &mut first);
    owner.cancel_wait(&mut owner.core.borrow_mut(), child);
    owner.cancel_operation(child);
    owner.drain_cleanups(child, &mut first);
    let mut core = owner.core.borrow_mut();
    let order = next_stamp(&mut core);
    core.tasks.get_mut(&child).unwrap().state = match first {
        Some(failure) => State::Faulted(failure, order),
        None => State::Cancelled(order),
    };
    // Keep the frame until generated code has rooted the outcome and releases
    // it. No resumption occurs between this operation and outcome extraction.
}
