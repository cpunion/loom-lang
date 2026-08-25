use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use polling::{Event, Events, Poller};

use crate::runtime::LoomRuntime;
use crate::scheduler::{LoomJoinSpec, LoomTask, WorkerCompletion};
use crate::{
    READY_COMPLETED, READY_READABLE, READY_TIMER, READY_WRITABLE, WAIT_ABI_VERSION,
    WAIT_DUPLICATE_SOURCE, WAIT_INFINITE, WAIT_INVALID_ARGUMENT, WAIT_NO_MEMORY, WAIT_OK,
    WAIT_READABLE, WAIT_SOURCE_COMPLETION, WAIT_SOURCE_FD, WAIT_SOURCE_TIMER,
    WAIT_STALE_REGISTRATION, WAIT_SYSTEM_ERROR, WAIT_WRITABLE,
};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LoomWaitSource {
    pub abi_version: u32,
    pub kind: u32,
    pub handle: i64,
    pub interests: u32,
    pub reserved: u32,
    pub deadline_ns: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LoomRegistration {
    pub key: u64,
    pub generation: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomReadyNotification {
    pub registration: LoomRegistration,
    pub frame: *mut c_void,
    pub events: u32,
    pub os_error: i32,
}

impl Default for LoomReadyNotification {
    fn default() -> Self {
        Self {
            registration: LoomRegistration::default(),
            frame: ptr::null_mut(),
            events: 0,
            os_error: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct RawSource(RawFd);

impl AsRawFd for RawSource {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl AsFd for RawSource {
    fn as_fd(&self) -> BorrowedFd<'_> {
        // SAFETY: every RawSource is used only while its registration entry is
        // alive. The ABI contract requires the caller to keep the descriptor
        // open until readiness, cancellation, or executor destruction.
        unsafe { BorrowedFd::borrow_raw(self.0) }
    }
}

#[derive(Clone, Copy)]
struct Entry {
    source: LoomWaitSource,
    registration: LoomRegistration,
    frame: *mut c_void,
}

pub(crate) struct Reactor {
    pub(crate) poller: Arc<Poller>,
    events: Events,
    entries: BTreeMap<u64, Entry>,
    ready: VecDeque<LoomReadyNotification>,
    next_key: u64,
    last_os_error: i32,
}

pub(crate) struct WorkerMailbox {
    pub(crate) sender: mpsc::Sender<WorkerCompletion>,
    pub(crate) receiver: mpsc::Receiver<WorkerCompletion>,
}

pub struct LoomExecutor {
    /// Stable runtime borrowed for this executor's entire lifetime. Legacy
    /// executors retain ownership in `owned_runtime`; v1 executors borrow a
    /// separately managed runtime whose attachment marker prevents early
    /// destruction or a second scheduler.
    pub(crate) runtime: NonNull<LoomRuntime>,
    owned_runtime: Option<Box<LoomRuntime>>,
    attached_to_runtime: bool,
    /// OS readiness is absent for synchronous roots and for async work that
    /// completes without suspending on a wait source.
    pub(crate) reactor: Option<Reactor>,
    pub(crate) tasks: Vec<Box<LoomTask>>,
    pub(crate) retired_tasks: Vec<*mut LoomTask>,
    pub(crate) runnable: VecDeque<*mut LoomTask>,
    pub(crate) active_task: *mut LoomTask,
    pub(crate) join_specs: Vec<Box<LoomJoinSpec>>,
    pub(crate) tasks_reclaimed: u64,
    /// The worker mailbox is needed only by blocking file/socket operations.
    pub(crate) worker: Option<WorkerMailbox>,
    reactor_init_os_error: i32,
}

/// Safe, cancellable owner for platform readiness registrations.
///
/// File descriptors are duplicated when registered, so closing or reusing the
/// caller's descriptor cannot invalidate a live registration.
pub struct WaitSet {
    id: u64,
    executor: Box<LoomExecutor>,
    descriptors: BTreeMap<u64, OwnedFd>,
}

/// Opaque identity of one live [`WaitSet`] registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitToken {
    set_id: u64,
    registration: LoomRegistration,
}

/// One readiness event returned by [`WaitSet::wait`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitEvent {
    pub token: WaitToken,
    pub events: u32,
    pub os_error: i32,
}

impl LoomExecutor {
    fn new(runtime: NonNull<LoomRuntime>, owned_runtime: Option<Box<LoomRuntime>>) -> Self {
        Self {
            runtime,
            owned_runtime,
            attached_to_runtime: false,
            reactor: None,
            tasks: Vec::new(),
            retired_tasks: Vec::new(),
            runnable: VecDeque::new(),
            active_task: ptr::null_mut(),
            join_specs: Vec::new(),
            tasks_reclaimed: 0,
            worker: None,
            reactor_init_os_error: 0,
        }
    }

    fn boxed_with_owned_runtime() -> Box<Self> {
        let mut runtime = Box::new(LoomRuntime::new());
        let runtime_pointer = NonNull::from(&mut *runtime);
        let mut executor = Box::new(Self::new(runtime_pointer, Some(runtime)));
        let executor_pointer = (&raw mut *executor).cast::<c_void>();
        // The executor is boxed before publishing either side of the
        // relationship, so both addresses remain stable until Drop detaches.
        let attached = unsafe { (*runtime_pointer.as_ptr()).try_attach_executor(executor_pointer) };
        debug_assert!(attached);
        executor.attached_to_runtime = attached;
        executor
    }

    unsafe fn boxed_for_runtime(runtime: *mut LoomRuntime) -> Option<Box<Self>> {
        let runtime_pointer = NonNull::new(runtime)?;
        let mut executor = Box::new(Self::new(runtime_pointer, None));
        let executor_pointer = (&raw mut *executor).cast::<c_void>();
        // SAFETY: callers provide a live, uniquely managed LoomRuntime. The
        // marker is updated before the executor pointer is returned.
        if !unsafe { (*runtime).try_attach_executor(executor_pointer) } {
            return None;
        }
        executor.attached_to_runtime = true;
        Some(executor)
    }

    pub(crate) fn runtime_pointer(&self) -> *mut LoomRuntime {
        self.runtime.as_ptr()
    }

    pub(crate) fn heap(&self) -> &crate::gc::LoomHeap {
        // SAFETY: attachment keeps the runtime live, and shared executor
        // access only requests shared heap access.
        unsafe { &(*self.runtime.as_ptr()).heap }
    }

    pub(crate) fn heap_mut(&mut self) -> &mut crate::gc::LoomHeap {
        // SAFETY: the executor is the only allowed attachment and `&mut self`
        // serializes mutation of its runtime heap.
        unsafe { &mut (*self.runtime.as_ptr()).heap }
    }

    fn ensure_reactor(&mut self) -> io::Result<&mut Reactor> {
        if self.reactor.is_none() {
            match Reactor::new() {
                Ok(reactor) => self.reactor = Some(reactor),
                Err(error) => {
                    self.reactor_init_os_error = error.raw_os_error().unwrap_or_default();
                    return Err(error);
                }
            }
        }
        Ok(self
            .reactor
            .as_mut()
            .expect("reactor was initialized immediately above"))
    }

    pub(crate) fn ensure_worker(&mut self) -> &mut WorkerMailbox {
        self.worker.get_or_insert_with(|| {
            let (sender, receiver) = mpsc::channel();
            WorkerMailbox { sender, receiver }
        })
    }
}

impl Drop for LoomExecutor {
    fn drop(&mut self) {
        if !self.attached_to_runtime {
            return;
        }
        let executor = (&raw mut *self).cast::<c_void>();
        // SAFETY: the runtime must outlive a borrowed executor. For a legacy
        // executor it is retained by owned_runtime until after this Drop body.
        unsafe { (*self.runtime.as_ptr()).detach_executor(executor) };
        self.attached_to_runtime = false;
        // Read the field so its ownership role remains explicit even though
        // Rust drops it automatically after this method returns.
        let _owns_runtime = self.owned_runtime.is_some();
    }
}

impl Reactor {
    fn new() -> io::Result<Self> {
        Ok(Self {
            poller: Arc::new(Poller::new()?),
            events: Events::new(),
            entries: BTreeMap::new(),
            ready: VecDeque::new(),
            next_key: 1,
            last_os_error: 0,
        })
    }
}

impl WaitSet {
    /// Allocates an empty readiness set.
    ///
    /// # Errors
    ///
    /// Returns the host error raised while creating the platform poller.
    pub fn new() -> io::Result<Self> {
        static NEXT_WAIT_SET_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_WAIT_SET_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| io::Error::other("Loom wait set identity space was exhausted"))?;
        let mut executor = LoomExecutor::boxed_with_owned_runtime();
        executor.ensure_reactor()?;
        Ok(Self {
            id,
            executor,
            descriptors: BTreeMap::new(),
        })
    }

    /// Registers one descriptor interest as a cancellable one-shot.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if descriptor duplication or poll registration
    /// fails, or if `interests` is not a supported descriptor interest.
    pub fn register_fd(&mut self, source: &impl AsFd, interests: u32) -> io::Result<WaitToken> {
        let descriptor = source.as_fd().try_clone_to_owned()?;
        let wait_source = LoomWaitSource {
            abi_version: WAIT_ABI_VERSION,
            kind: WAIT_SOURCE_FD,
            handle: i64::from(descriptor.as_raw_fd()),
            interests,
            reserved: 0,
            deadline_ns: 0,
        };
        let mut registration = LoomRegistration::default();
        let frame = NonNull::<u8>::dangling().as_ptr().cast::<c_void>();
        // SAFETY: the boxed executor has a stable address; wait_source and the
        // out pointer live for this call. The duplicated descriptor remains
        // owned below until readiness or cancellation. frame is opaque.
        let status = unsafe {
            executor_register(
                &raw mut *self.executor,
                &raw const wait_source,
                frame,
                &raw mut registration,
            )
        };
        if status == WAIT_OK {
            self.descriptors.insert(registration.key, descriptor);
            Ok(WaitToken {
                set_id: self.id,
                registration,
            })
        } else {
            Err(self.status_error("register", status))
        }
    }

    /// Waits until one or more registrations are ready or `timeout` expires.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the platform poller cannot wait.
    pub fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Vec<WaitEvent>> {
        let timeout_ns = timeout.map_or(WAIT_INFINITE, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX - 1)
        });
        let mut ready_count = 0;
        // SAFETY: the executor and unique out pointer are live for this call.
        let status =
            unsafe { executor_wait(&raw mut *self.executor, timeout_ns, &raw mut ready_count) };
        if status != WAIT_OK {
            return Err(self.status_error("wait", status));
        }
        let capacity = usize::try_from(ready_count)
            .unwrap_or(usize::MAX)
            .min(self.descriptors.len());
        let mut events = Vec::with_capacity(capacity);
        let ready_limit =
            ready_count.min(u32::try_from(self.descriptors.len()).unwrap_or(u32::MAX));
        for _ in 0..ready_limit {
            let mut notification = LoomReadyNotification::default();
            // SAFETY: the executor and unique out pointer are live for this call.
            let popped =
                unsafe { executor_pop_ready(&raw mut *self.executor, &raw mut notification) };
            if popped == 0 {
                break;
            }
            if popped < 0 {
                return Err(self.status_error("pop ready", -popped));
            }
            self.descriptors.remove(&notification.registration.key);
            events.push(WaitEvent {
                token: WaitToken {
                    set_id: self.id,
                    registration: notification.registration,
                },
                events: notification.events,
                os_error: notification.os_error,
            });
        }
        Ok(events)
    }

    /// Cancels one live registration.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if `token` is stale or belongs to another set.
    pub fn cancel(&mut self, token: WaitToken) -> io::Result<()> {
        if token.set_id != self.id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "wait token belongs to another wait set",
            ));
        }
        // SAFETY: the boxed executor and token registration are live.
        let status =
            unsafe { executor_cancel(&raw mut *self.executor, &raw const token.registration) };
        if status == WAIT_OK {
            self.descriptors.remove(&token.registration.key);
            Ok(())
        } else {
            Err(self.status_error("cancel", status))
        }
    }

    fn status_error(&self, operation: &str, status: i32) -> io::Error {
        let os_error = self
            .executor
            .reactor
            .as_ref()
            .map_or(self.executor.reactor_init_os_error, |reactor| {
                reactor.last_os_error
            });
        if status == WAIT_SYSTEM_ERROR && os_error != 0 {
            return io::Error::from_raw_os_error(os_error);
        }
        let kind = if status == WAIT_INVALID_ARGUMENT || status == WAIT_DUPLICATE_SOURCE {
            io::ErrorKind::InvalidInput
        } else if status == WAIT_STALE_REGISTRATION {
            io::ErrorKind::NotFound
        } else if status == WAIT_NO_MEMORY {
            io::ErrorKind::OutOfMemory
        } else {
            io::ErrorKind::Other
        };
        io::Error::new(
            kind,
            format!("Loom wait set {operation} failed with status {status}"),
        )
    }
}

impl Drop for Reactor {
    fn drop(&mut self) {
        for entry in self.entries.values() {
            if entry.source.kind == WAIT_SOURCE_FD
                && let Ok(descriptor) = RawFd::try_from(entry.source.handle)
            {
                let _ = self.poller.delete(RawSource(descriptor));
            }
        }
    }
}

fn remember_error(reactor: &mut Reactor, error: &io::Error) {
    reactor.last_os_error = error.raw_os_error().unwrap_or_default();
}

fn clock_origin() -> &'static Instant {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    ORIGIN.get_or_init(Instant::now)
}

fn now_ns() -> u64 {
    u64::try_from(clock_origin().elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn valid_source(source: &LoomWaitSource) -> bool {
    if source.abi_version != WAIT_ABI_VERSION || source.reserved != 0 {
        return false;
    }
    match source.kind {
        WAIT_SOURCE_TIMER | WAIT_SOURCE_COMPLETION => source.interests == 0,
        WAIT_SOURCE_FD => {
            source.handle >= 0
                && source.handle <= i64::from(i32::MAX)
                && source.interests != 0
                && source.interests & !(WAIT_READABLE | WAIT_WRITABLE) == 0
        }
        _ => false,
    }
}

fn registration_exists(reactor: &Reactor, registration: LoomRegistration) -> bool {
    reactor
        .entries
        .get(&registration.key)
        .is_some_and(|entry| entry.registration == registration)
}

fn remove_entry(reactor: &mut Reactor, key: u64) -> Option<Entry> {
    let entry = reactor.entries.remove(&key)?;
    if entry.source.kind == WAIT_SOURCE_FD
        && let Ok(descriptor) = RawFd::try_from(entry.source.handle)
        && let Err(error) = reactor.poller.delete(RawSource(descriptor))
    {
        remember_error(reactor, &error);
    }
    Some(entry)
}

fn enqueue(reactor: &mut Reactor, entry: Entry, events: u32, os_error: i32) {
    reactor.ready.push_back(LoomReadyNotification {
        registration: entry.registration,
        frame: entry.frame,
        events,
        os_error,
    });
}

fn collect_expired_timers(reactor: &mut Reactor) {
    let now = now_ns();
    let expired = reactor
        .entries
        .iter()
        .filter_map(|(key, entry)| {
            (entry.source.kind == WAIT_SOURCE_TIMER && entry.source.deadline_ns <= now)
                .then_some(*key)
        })
        .collect::<Vec<_>>();
    for key in expired {
        if let Some(entry) = remove_entry(reactor, key) {
            enqueue(reactor, entry, READY_TIMER, 0);
        }
    }
}

fn effective_timeout(reactor: &Reactor, timeout_ns: u64) -> Option<Duration> {
    let requested = (timeout_ns != WAIT_INFINITE).then(|| Duration::from_nanos(timeout_ns));
    let now = now_ns();
    let timer = reactor
        .entries
        .values()
        .filter(|entry| entry.source.kind == WAIT_SOURCE_TIMER)
        .map(|entry| Duration::from_nanos(entry.source.deadline_ns.saturating_sub(now)))
        .min();
    match (requested, timer) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[unsafe(export_name = "loom_wait_now_ns")]
pub extern "C" fn wait_now_ns() -> u64 {
    // This public timestamp only needs to share a monotonic domain inside one
    // process. A process-wide origin makes sources portable across executors.
    now_ns()
}

#[unsafe(export_name = "loom_executor_create")]
pub extern "C" fn executor_create() -> *mut LoomExecutor {
    Box::into_raw(LoomExecutor::boxed_with_owned_runtime())
}

#[unsafe(export_name = "loom_executor_create_for_runtime_v1")]
pub unsafe extern "C" fn executor_create_for_runtime_v1(
    runtime: *mut LoomRuntime,
) -> *mut LoomExecutor {
    // SAFETY: the runtime lifetime/ownership contract is part of this v1 ABI.
    unsafe { LoomExecutor::boxed_for_runtime(runtime) }.map_or(ptr::null_mut(), Box::into_raw)
}

#[unsafe(export_name = "loom_executor_runtime_v1")]
pub unsafe extern "C" fn executor_runtime_v1(executor: *mut LoomExecutor) -> *mut LoomRuntime {
    if executor.is_null() {
        ptr::null_mut()
    } else {
        // SAFETY: the pointer was checked and is borrowed only for this read.
        unsafe { (*executor).runtime_pointer() }
    }
}

#[unsafe(export_name = "loom_executor_destroy")]
pub unsafe extern "C" fn executor_destroy(executor: *mut LoomExecutor) {
    if !executor.is_null() {
        // SAFETY: ownership of this pointer was returned by executor_create and
        // the ABI requires exactly one matching destroy call.
        drop(unsafe { Box::from_raw(executor) });
    }
}

#[unsafe(export_name = "loom_executor_register")]
pub unsafe extern "C" fn executor_register(
    executor: *mut LoomExecutor,
    source: *const LoomWaitSource,
    frame: *mut c_void,
    registration_out: *mut LoomRegistration,
) -> i32 {
    if executor.is_null() || source.is_null() || frame.is_null() || registration_out.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    // SAFETY: all pointers were checked and are borrowed only for this call.
    let executor = unsafe { &mut *executor };
    let source = unsafe { *source };
    if !valid_source(&source) {
        return WAIT_INVALID_ARGUMENT;
    }
    let Ok(reactor) = executor.ensure_reactor() else {
        return WAIT_SYSTEM_ERROR;
    };
    if source.kind == WAIT_SOURCE_FD
        && reactor.entries.values().any(|entry| {
            entry.source.kind == WAIT_SOURCE_FD
                && entry.source.handle == source.handle
                && entry.source.interests & source.interests != 0
        })
    {
        return WAIT_DUPLICATE_SOURCE;
    }
    let key = reactor.next_key;
    let Ok(event_key) = usize::try_from(key) else {
        return WAIT_NO_MEMORY;
    };
    if key == 0 || event_key == usize::MAX {
        return WAIT_NO_MEMORY;
    }
    reactor.next_key = key.wrapping_add(1);
    let registration = LoomRegistration { key, generation: 1 };
    if source.kind == WAIT_SOURCE_FD {
        let Ok(descriptor) = RawFd::try_from(source.handle) else {
            return WAIT_INVALID_ARGUMENT;
        };
        let mut event = Event::none(event_key);
        event.readable = source.interests & WAIT_READABLE != 0;
        event.writable = source.interests & WAIT_WRITABLE != 0;
        // SAFETY: the registration entry below retains the raw descriptor and
        // the ABI requires it to remain open until this one-shot is removed.
        if let Err(error) = unsafe { reactor.poller.add(&RawSource(descriptor), event) } {
            remember_error(reactor, &error);
            return WAIT_SYSTEM_ERROR;
        }
    }
    reactor.entries.insert(
        key,
        Entry {
            source,
            registration,
            frame,
        },
    );
    // SAFETY: registration_out is a valid unique out pointer for this call.
    unsafe { registration_out.write(registration) };
    WAIT_OK
}

#[unsafe(export_name = "loom_executor_cancel")]
pub unsafe extern "C" fn executor_cancel(
    executor: *mut LoomExecutor,
    registration: *const LoomRegistration,
) -> i32 {
    if executor.is_null() || registration.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    // SAFETY: pointers were checked and remain borrowed for this call only.
    let executor = unsafe { &mut *executor };
    let registration = unsafe { *registration };
    let Some(reactor) = executor.reactor.as_mut() else {
        return WAIT_STALE_REGISTRATION;
    };
    if !registration_exists(reactor, registration) {
        return WAIT_STALE_REGISTRATION;
    }
    remove_entry(reactor, registration.key);
    WAIT_OK
}

#[unsafe(export_name = "loom_executor_notify_completion")]
pub unsafe extern "C" fn executor_notify_completion(
    executor: *mut LoomExecutor,
    registration: *const LoomRegistration,
    events: u32,
    os_error: i32,
) -> i32 {
    if executor.is_null() || registration.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    // SAFETY: pointers were checked and remain borrowed for this call only.
    let executor = unsafe { &mut *executor };
    let registration = unsafe { *registration };
    let Some(reactor) = executor.reactor.as_mut() else {
        return WAIT_STALE_REGISTRATION;
    };
    let Some(entry) = reactor.entries.get(&registration.key).copied() else {
        return WAIT_STALE_REGISTRATION;
    };
    if entry.registration != registration || entry.source.kind != WAIT_SOURCE_COMPLETION {
        return WAIT_STALE_REGISTRATION;
    }
    remove_entry(reactor, registration.key);
    enqueue(reactor, entry, events | READY_COMPLETED, os_error);
    WAIT_OK
}

#[unsafe(export_name = "loom_executor_wait")]
pub unsafe extern "C" fn executor_wait(
    executor: *mut LoomExecutor,
    timeout_ns: u64,
    ready_count_out: *mut u32,
) -> i32 {
    if executor.is_null() || ready_count_out.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    // SAFETY: pointers were checked and remain borrowed for this call only.
    let executor = unsafe { &mut *executor };
    let Ok(reactor) = executor.ensure_reactor() else {
        return WAIT_SYSTEM_ERROR;
    };
    collect_expired_timers(reactor);
    if reactor.ready.is_empty() {
        let timeout = effective_timeout(reactor, timeout_ns);
        reactor.events.clear();
        if let Err(error) = reactor.poller.wait(&mut reactor.events, timeout) {
            remember_error(reactor, &error);
            return WAIT_SYSTEM_ERROR;
        }
        let events = reactor.events.iter().collect::<Vec<_>>();
        for event in events {
            let Ok(key) = u64::try_from(event.key) else {
                continue;
            };
            let Some(entry) = remove_entry(reactor, key) else {
                continue;
            };
            let mut ready = 0;
            if event.readable {
                ready |= READY_READABLE;
            }
            if event.writable {
                ready |= READY_WRITABLE;
            }
            enqueue(reactor, entry, ready, 0);
        }
        collect_expired_timers(reactor);
    }
    let count = u32::try_from(reactor.ready.len()).unwrap_or(u32::MAX);
    // SAFETY: ready_count_out is a valid unique out pointer for this call.
    unsafe { ready_count_out.write(count) };
    WAIT_OK
}

#[unsafe(export_name = "loom_executor_pop_ready")]
pub unsafe extern "C" fn executor_pop_ready(
    executor: *mut LoomExecutor,
    notification_out: *mut LoomReadyNotification,
) -> i32 {
    if executor.is_null() || notification_out.is_null() {
        return -WAIT_INVALID_ARGUMENT;
    }
    // SAFETY: pointers were checked and remain borrowed for this call only.
    let executor = unsafe { &mut *executor };
    let Some(reactor) = executor.reactor.as_mut() else {
        return 0;
    };
    let Some(notification) = reactor.ready.pop_front() else {
        return 0;
    };
    // SAFETY: notification_out is a valid unique out pointer for this call.
    unsafe { notification_out.write(notification) };
    1
}

#[unsafe(export_name = "loom_executor_last_os_error")]
pub unsafe extern "C" fn executor_last_os_error(executor: *const LoomExecutor) -> i32 {
    if executor.is_null() {
        return 0;
    }
    // SAFETY: the non-null executor is borrowed immutably for this call.
    let executor = unsafe { &*executor };
    executor
        .reactor
        .as_ref()
        .map_or(executor.reactor_init_os_error, |reactor| {
            reactor.last_os_error
        })
}

pub(crate) unsafe fn register_for_task(
    executor: *mut LoomExecutor,
    source: *const LoomWaitSource,
    frame: *mut c_void,
    registration: *mut LoomRegistration,
) -> i32 {
    // SAFETY: scheduler validated the same ABI pointers before forwarding.
    unsafe { executor_register(executor, source, frame, registration) }
}

pub(crate) unsafe fn cancel_for_task(
    executor: *mut LoomExecutor,
    registration: *const LoomRegistration,
) -> i32 {
    // SAFETY: registration belongs to this executor's task.
    unsafe { executor_cancel(executor, registration) }
}

pub(crate) unsafe fn wait_for_scheduler(executor: *mut LoomExecutor, ready_count: *mut u32) -> i32 {
    // SAFETY: scheduler owns both pointers for the duration of the call.
    unsafe { executor_wait(executor, WAIT_INFINITE, ready_count) }
}

pub(crate) unsafe fn pop_for_scheduler(
    executor: *mut LoomExecutor,
    notification: *mut LoomReadyNotification,
) -> i32 {
    // SAFETY: scheduler owns both pointers for the duration of the call.
    unsafe { executor_pop_ready(executor, notification) }
}

pub(crate) fn has_registrations(executor: &LoomExecutor) -> bool {
    executor
        .reactor
        .as_ref()
        .is_some_and(|reactor| !reactor.entries.is_empty())
}

/// Blocks for one readiness event on a borrowed process descriptor.
///
/// This is the interpreter-side oracle for the same `polling` reactor used by
/// native executors. The descriptor is never closed or otherwise owned here.
///
/// # Errors
///
/// Returns an invalid-input error for malformed descriptors/interests and
/// forwards errors from the operating-system poller.
pub fn wait_fd_once(handle: i64, interests: u32) -> io::Result<u32> {
    if handle < 0
        || handle > i64::from(i32::MAX)
        || interests == 0
        || interests & !(WAIT_READABLE | WAIT_WRITABLE) != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Loom fd wait source",
        ));
    }
    let descriptor = RawFd::try_from(handle).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Loom fd does not fit the platform descriptor type",
        )
    })?;
    let source = RawSource(descriptor);
    let poller = Poller::new()?;
    let mut event = Event::none(0);
    event.readable = interests & WAIT_READABLE != 0;
    event.writable = interests & WAIT_WRITABLE != 0;
    // SAFETY: RawSource does not own or close the descriptor and remains live
    // until the one-shot registration is deleted below.
    unsafe { poller.add(&source, event)? };
    let mut events = Events::new();
    let result = poller.wait(&mut events, None);
    let deleted = poller.delete(source);
    result?;
    deleted?;
    let mut ready = 0;
    for event in events.iter() {
        if event.readable {
            ready |= READY_READABLE;
        }
        if event.writable {
            ready |= READY_WRITABLE;
        }
    }
    Ok(ready)
}

#[cfg(test)]
mod wait_set_tests {
    use std::ffi::c_void;
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn synchronous_heap_use_does_not_initialize_async_resources() {
        let executor = executor_create();
        assert!(!executor.is_null());
        unsafe {
            assert!((*executor).owned_runtime.is_some());
            assert_eq!(executor_runtime_v1(executor), (*executor).runtime_pointer());
            assert!((*executor).reactor.is_none());
            assert!((*executor).worker.is_none());
            assert_eq!(crate::gc::activate_executor(executor), WAIT_OK);
            assert!(!crate::gc::allocate_value().is_null());
            assert!(!crate::gc::allocate_value_node().is_null());
            assert_eq!(crate::gc::deactivate_executor(executor), WAIT_OK);
            assert!((*executor).reactor.is_none());
            assert!((*executor).worker.is_none());
            executor_destroy(executor);
        }
    }

    #[test]
    fn first_registration_initializes_one_reactor_but_no_worker_mailbox() {
        let executor = executor_create();
        assert!(!executor.is_null());
        let source = LoomWaitSource {
            abi_version: WAIT_ABI_VERSION,
            kind: WAIT_SOURCE_COMPLETION,
            handle: -1,
            ..LoomWaitSource::default()
        };
        let frame = NonNull::<u8>::dangling().as_ptr().cast::<c_void>();
        let mut first = LoomRegistration::default();
        let mut second = LoomRegistration::default();
        unsafe {
            assert_eq!(
                executor_register(executor, &raw const source, frame, &raw mut first),
                WAIT_OK
            );
            let poller = Arc::as_ptr(&(*executor).reactor.as_ref().unwrap().poller);
            assert!((*executor).worker.is_none());
            assert_eq!(
                executor_register(executor, &raw const source, frame, &raw mut second),
                WAIT_OK
            );
            assert_eq!(
                Arc::as_ptr(&(*executor).reactor.as_ref().unwrap().poller),
                poller
            );
            assert!((*executor).worker.is_none());
            assert_eq!(executor_cancel(executor, &raw const first), WAIT_OK);
            assert_eq!(executor_cancel(executor, &raw const second), WAIT_OK);
            executor_destroy(executor);
        }
    }

    #[test]
    fn token_from_another_wait_set_cannot_cancel_a_colliding_registration() {
        let (source, _peer) = UnixStream::pair().expect("create wait-set fixture");
        let mut left = WaitSet::new().expect("create left wait set");
        let mut right = WaitSet::new().expect("create right wait set");
        let left_token = left
            .register_fd(&source, WAIT_READABLE)
            .expect("register left source");
        let right_token = right
            .register_fd(&source, WAIT_READABLE)
            .expect("register right source");
        assert_eq!(
            left_token.registration.key, right_token.registration.key,
            "the regression requires colliding executor-local keys"
        );
        assert_ne!(left_token.set_id, right_token.set_id);

        let error = right
            .cancel(left_token)
            .expect_err("foreign token must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        right.cancel(right_token).expect("cancel right token");
        left.cancel(left_token).expect("cancel left token");
    }
}
