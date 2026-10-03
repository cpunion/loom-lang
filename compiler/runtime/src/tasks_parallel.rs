use super::*;

#[unsafe(no_mangle)]
unsafe extern "C-unwind" fn loom_rt_task_wait_worker(
    frame: *mut u8,
    callback: crate::parallel::Callback,
) -> i32 {
    let (started, complete) = edit(|owner, core| {
        let id = core.current.ok_or("worker submission outside a resume")?;
        let complete = core.tasks[&id]
            .computation
            .as_ref()
            .is_some_and(|job| job.complete());
        if complete {
            owner.cancel_wait(core, id);
            core.tasks.get_mut(&id).unwrap().state = State::Running;
        }
        Ok((core.tasks[&id].computation.is_some(), complete))
    });
    if complete {
        return 1;
    }
    let ready = unsafe {
        wait_source(
            WaitSource {
                kind: KIND_COMPLETION,
                interests: 0,
                handle: 0,
                deadline_ns: 0,
            },
            None,
        )
    };
    if started {
        return i32::from(ready.is_some());
    }
    let result = edit(|owner, core| {
        if owner.parallel.get().is_none() {
            match crate::parallel::Pool::new() {
                Ok(pool) => {
                    let _ = owner.parallel.set(pool);
                }
                Err(error) => return Ok(Err(error)),
            }
        }
        let id = core.current.ok_or("worker submission outside a resume")?;
        let task = core.tasks.get_mut(&id).unwrap();
        let registration = task
            .external
            .as_ref()
            .ok_or("worker needs completion registration")?
            .registration;
        task.computation = Some(unsafe {
            owner.parallel.get().unwrap().submit(
                frame,
                callback,
                Arc::clone(owner.reactor.get().unwrap()),
                registration,
            )
        });
        Ok(Ok(()))
    });
    result.unwrap_or_else(|error| wait_fault(error));
    0
}

// Explicit cancellation preserves a callback that completed before observing
// cancellation. Resume its private wrapper to materialize the typed outcome;
// never label an actual result or fault as Cancelled.
pub(super) fn complete_before_cancel(owner: &Owner, id: u64) -> bool {
    let job = owner.core.borrow().tasks[&id].computation.clone();
    let Some(job) = job else { return false };
    if !job.complete_before_cancel() {
        return false;
    }
    let (parent, waiter, frame, resume) = {
        let mut core = owner.core.borrow_mut();
        let parent = core.current.replace(id);
        core.ready.retain(|queued| *queued != id);
        let task = core.tasks.get_mut(&id).unwrap();
        task.state = State::Running;
        (parent, task.waiter.take(), task.frame, task.resume)
    };
    unsafe {
        owner.resume_task(id, frame, resume);
    }
    let mut core = owner.core.borrow_mut();
    core.current = parent;
    let task = core.tasks.get_mut(&id).unwrap();
    task.waiter = waiter;
    if task.state.terminal_order().is_none() {
        fatal("worker completion wrapper suspended");
    }
    true
}

#[unsafe(no_mangle)]
extern "C-unwind" fn loom_rt_task_worker_result() {
    let job = edit(|_, core| {
        let id = core.current.ok_or("worker result outside a resume")?;
        let task = core.tasks.get_mut(&id).unwrap();
        if !matches!(task.state, State::Running) || task.external.is_some() {
            return Err("worker result requires completed wait");
        }
        task.computation
            .take()
            .ok_or("worker result already consumed")
    });
    if let Err(failure) = job.take() {
        raise_owned(failure);
    }
}
