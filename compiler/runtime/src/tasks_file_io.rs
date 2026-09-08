use super::*;

fn file_wait(prepare: impl FnOnce() -> Operation) -> i32 {
    let started = edit(|_, core| {
        let id = core.current.ok_or("file operation outside a resume")?;
        Ok(core.tasks[&id].operation.is_some())
    });
    // Completion has no borrowed native handle. Job owns an independent File
    // duplicate; the owner still drains it before closing the source handle.
    let ready = unsafe {
        wait_source(WaitSource {
            kind: KIND_COMPLETION,
            interests: 0,
            handle: 0,
            deadline_ns: 0,
        })
    };
    if started {
        return i32::from(ready.is_some());
    }
    let submitted = edit(|owner, core| {
        let id = core.current.ok_or("file operation outside a resume")?;
        let task = core.tasks.get_mut(&id).unwrap();
        let wait = task
            .external
            .ok_or("file operation needs completion registration")?;
        let workers = match owner.workers() {
            Ok(workers) => workers,
            Err(error) => return Ok(Err(error)),
        };
        let job = workers.submit(
            prepare(),
            owner.reactor.get().unwrap().clone(),
            wait.registration,
        );
        task.operation = Some(job);
        Ok(Ok(()))
    });
    submitted.unwrap_or_else(|error| wait_fault(error));
    0
}

fn completed() -> Outcome {
    let job = edit(|_, core| {
        let id = core.current.ok_or("file result outside a resume")?;
        let task = core.tasks.get_mut(&id).unwrap();
        if !matches!(task.state, State::Running) || task.external.is_some() {
            return Err("file result requires completed wait");
        }
        task.operation.take().ok_or("file result already consumed")
    });
    job.take()
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_wait_file_read(fd: i64, limit: i64) -> i32 {
    file_wait(
        || match (super::super::file_io::duplicate(fd), usize::try_from(limit)) {
            (Ok(file), Ok(limit)) => Operation::Read(file, limit),
            _ => Operation::Failed,
        },
    )
}

fn write_operation(fd: i64, bytes: &[u8], offset: i64) -> Operation {
    match (
        super::super::file_io::duplicate(fd),
        usize::try_from(offset)
            .ok()
            .and_then(|offset| bytes.get(offset..)),
    ) {
        (Ok(file), Some(bytes)) => Operation::Write(file, bytes.to_vec()),
        _ => Operation::Failed,
    }
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_task_wait_file_write(
    fd: i64,
    text: *const u8,
    offset: i64,
) -> i32 {
    // SAFETY: The caller supplies a rooted Text. Snapshot only at first start;
    // native workers never retain the pointer or re-read shared input on resume.
    file_wait(|| write_operation(fd, unsafe { super::super::text_bytes(text) }, offset))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_task_wait_file_write_bytes(
    fd: i64,
    bytes: *const u8,
    offset: i64,
) -> i32 {
    // SAFETY: As above, for a rooted Bytes header/backing store.
    file_wait(|| write_operation(fd, unsafe { super::super::buffer_bytes(bytes) }, offset))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_file_result() -> i64 {
    completed().count
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_task_file_read_result(bytes: *mut u8) -> i64 {
    let outcome = completed();
    if !outcome.bytes.is_empty() {
        // SAFETY: The caller roots Bytes. reserve reloads its moved header;
        // native output remains independent across collection and the copy.
        unsafe {
            let buffer = super::super::reserve(bytes, outcome.bytes.len(), 1);
            ptr::copy_nonoverlapping(
                outcome.bytes.as_ptr(),
                (*buffer).data.add((*buffer).len),
                outcome.bytes.len(),
            );
            (*buffer).len += outcome.bytes.len();
        }
    }
    outcome.count
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    thread_local! {
        static FINISHED: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    }

    unsafe extern "C-unwind" fn cleaned(_: *mut u8) {
        FINISHED.with(|value| assert!(value.borrow().as_ref().unwrap().load(Ordering::SeqCst)));
        let owner = unsafe { &*OWNER.get() };
        let core = owner.core.borrow();
        assert_eq!(core.pending, 0);
        assert!(
            core.tasks
                .values()
                .all(|task| task.operation.is_none() && task.external.is_none())
        );
    }

    unsafe extern "C-unwind" fn resume(_: *mut u8) -> i64 {
        loom_rt_task_cleanup_push(0, cleaned);
        let finished = FINISHED.with(|value| value.borrow().as_ref().unwrap().clone());
        let (started, running) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        assert_eq!(
            file_wait(|| Operation::Test(Box::new(move || {
                started.send(()).unwrap();
                gate.recv().unwrap();
                finished.store(true, Ordering::SeqCst);
                Outcome {
                    count: 1,
                    bytes: vec![1],
                }
            }))),
            0
        );
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            release.send(()).unwrap();
        });
        fault("fault after file registration");
    }

    unsafe extern "C-unwind" fn construct() -> u64 {
        let frame = crate::loom_rt_box_new(16, None);
        unsafe { loom_rt_task_create(frame, resume, ptr::null(), 0) }
    }

    #[test]
    fn fault_before_pending_drains_running_io_before_frame_cleanup() {
        FINISHED.with(|value| *value.borrow_mut() = Some(Arc::new(AtomicBool::new(false))));
        let failure = unsafe { catch_fault(|| loom_rt_task_run(construct)) }.unwrap_err();
        assert_eq!(failure.message, b"fault after file registration");
        assert!(OWNER.get().is_null());
        FINISHED.with(|value| *value.borrow_mut() = None);
    }
}
