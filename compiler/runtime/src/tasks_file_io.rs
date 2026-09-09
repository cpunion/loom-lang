use super::*;
use crate::blocking_io::{Pool, close_file};
use std::fs::File;

// Tokens are private, owner-local and never reused. The table retains native
// ownership between operations; neither GC nor an OS descriptor is its identity.
pub(super) struct Files {
    pool: Pool,
    handles: RefCell<HashMap<i64, File>>,
    next: Cell<i64>,
}

impl Files {
    pub(super) fn new(count: usize) -> io::Result<Self> {
        Ok(Self {
            pool: Pool::new(count)?,
            handles: RefCell::new(HashMap::new()),
            next: Cell::new(0),
        })
    }

    fn insert(&self, file: File) -> i64 {
        let token = self
            .next
            .get()
            .checked_add(1)
            .unwrap_or_else(|| fatal("file identities exhausted"));
        self.handles.borrow_mut().insert(token, file);
        self.next.set(token);
        token
    }

    fn duplicate(&self, token: i64) -> io::Result<File> {
        self.handles
            .borrow()
            .get(&token)
            .ok_or(io::ErrorKind::InvalidInput)?
            .try_clone()
    }

    fn take(&self, token: i64) -> Option<File> {
        self.handles.borrow_mut().remove(&token)
    }
}

fn duplicate(token: i64) -> io::Result<File> {
    edit(|owner, _| {
        Ok(owner
            .workers
            .get()
            .ok_or(io::ErrorKind::InvalidInput)
            .map_err(io::Error::from)
            .and_then(|files| files.duplicate(token)))
    })
}

fn take(token: i64) -> Option<File> {
    edit(|owner, _| Ok(owner.workers.get().and_then(|files| files.take(token))))
}

fn file_wait(prepare: impl FnOnce() -> Operation) -> i32 {
    let started = edit(|_, core| {
        let id = core.current.ok_or("file operation outside a resume")?;
        Ok(core.tasks[&id].operation.is_some())
    });
    // Prepare before any fallible reactor/pool setup. In particular close has
    // already taken ownership when source cleanup relinquishes its handle.
    let operation = if started { None } else { Some(prepare()) };
    // Completion has no borrowed native handle. Job owns its inputs/results;
    // the owner drains it before running source cleanup.
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
        let job = workers.pool.submit(
            operation.unwrap(),
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
    file_wait(|| match (duplicate(fd), usize::try_from(limit)) {
        (Ok(file), Ok(limit)) => Operation::Read(file, limit),
        _ => Operation::Failed,
    })
}

fn write_operation(fd: i64, bytes: &[u8], offset: i64) -> Operation {
    match (
        duplicate(fd),
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
pub(super) unsafe extern "C-unwind" fn loom_rt_task_wait_file_open(
    path: *const u8,
    create: i32,
) -> i32 {
    // SAFETY: Copy a live UTF-8 Text before submitting work; no pointer escapes.
    file_wait(|| {
        Operation::Open(
            unsafe { std::str::from_utf8_unchecked(crate::text_bytes(path)) }.to_owned(),
            create != 0,
        )
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_file_open_result() -> i64 {
    let outcome = completed();
    match outcome.file {
        Some(file) => edit(|owner, _| Ok(owner.workers.get().unwrap().insert(file))),
        None => -1,
    }
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_wait_file_close(token: i64) -> i32 {
    file_wait(|| take(token).map_or(Operation::Failed, Operation::Close))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_file_abort(token: i64) -> i64 {
    // Cleanup cannot suspend. Normal close uses a worker; failure fallback
    // closes synchronously only after all pending operations have drained.
    take(token).map_or(-1, close_file)
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

    #[test]
    fn file_tokens_transfer_once_and_do_not_alias_reused_handles() {
        let files = Files::new(1).unwrap();
        let first = files.insert(tempfile::tempfile().unwrap());
        drop(files.duplicate(first).unwrap());
        let close = Operation::Close(files.take(first).unwrap());
        assert!(files.take(first).is_none());
        assert!(files.duplicate(first).is_err());
        drop(close); // Cancellation before submission still releases ownership.
        let second = files.insert(tempfile::tempfile().unwrap());
        assert_ne!(first, second);
        assert!(files.take(first).is_none());
        assert!(files.duplicate(second).is_ok());
        assert_eq!(close_file(files.take(second).unwrap()), 0);
        assert!(files.handles.borrow().is_empty());
    }

    unsafe extern "C-unwind" fn cleaned(_: *mut u8) {
        FINISHED.with(|value| assert!(value.borrow().as_ref().unwrap().load(Ordering::SeqCst)));
        let owner = unsafe { &*OWNER.get() };
        assert!(owner.workers.get().unwrap().handles.borrow().is_empty());
        let core = owner.core.borrow();
        assert_eq!(core.pending, 0);
        assert!(
            core.tasks
                .values()
                .all(|task| task.operation.is_none() && task.external.is_none())
        );
    }

    fn start_blocking() -> mpsc::Sender<()> {
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
                    file: Some(tempfile::tempfile().unwrap()),
                }
            }))),
            0
        );
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        release
    }

    fn finish_later(release: mpsc::Sender<()>) {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            release.send(()).unwrap();
        });
    }

    unsafe extern "C-unwind" fn resume(_: *mut u8) -> i64 {
        finish_later(start_blocking());
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

    unsafe extern "C-unwind" fn cancel_running(_: *mut u8) -> i64 {
        let owner = unsafe { &*OWNER.get() };
        let parent = owner.core.borrow().current.unwrap();
        let child = unsafe { construct() };
        // Focus on the drain boundary: submit a real worker operation under
        // the child's activation, then cancel from its running parent. Source
        // tests separately exercise normal coroutine scheduling and suspension.
        {
            let mut core = owner.core.borrow_mut();
            core.current = Some(child);
            core.tasks.get_mut(&child).unwrap().state = State::Running;
        }
        let release = start_blocking();
        owner.core.borrow_mut().current = Some(parent);
        FINISHED.with(|value| assert!(!value.borrow().as_ref().unwrap().load(Ordering::SeqCst)));
        finish_later(release);
        outcomes::loom_rt_task_cancel_begin(child);
        assert_eq!(outcomes::loom_rt_task_status(child), 2);
        loom_rt_task_release(child);
        assert!(owner.core.borrow().tasks[&parent].children.is_empty());
        0
    }

    unsafe extern "C-unwind" fn cancel_constructor() -> u64 {
        let frame = crate::loom_rt_box_new(16, None);
        unsafe { loom_rt_task_create(frame, cancel_running, ptr::null(), 0) }
    }

    #[test]
    fn explicit_cancellation_is_terminal_only_after_running_work_and_cleanup() {
        FINISHED.with(|value| *value.borrow_mut() = Some(Arc::new(AtomicBool::new(false))));
        unsafe { loom_rt_task_run(cancel_constructor) };
        assert!(OWNER.get().is_null());
        FINISHED.with(|value| *value.borrow_mut() = None);
    }
}
