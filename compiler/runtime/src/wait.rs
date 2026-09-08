//! Portable, one-shot wait registration. Notifications contain identities only;
//! this boundary neither retains managed pointers nor executes Loom callbacks.

use polling::{Event, Events, Poller};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

pub(crate) const KIND_TIMER: u32 = 1;
pub(crate) const KIND_READINESS: u32 = 2;
pub(crate) const KIND_COMPLETION: u32 = 3;
pub(crate) const READABLE: u32 = 1;
pub(crate) const WRITABLE: u32 = 2;
pub(crate) const TIMER: u32 = 4;
pub(crate) const COMPLETION: u32 = 8;
pub(crate) const ERROR: u32 = 16;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct WaitSource {
    pub kind: u32,
    pub interests: u32,
    pub handle: u64,
    pub deadline_ns: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Registration {
    pub key: u64,
    pub generation: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReadyNotification {
    pub registration: Registration,
    pub owner: u64,
    pub events: u32,
    pub os_error: i32,
}

#[derive(Clone, Copy)]
struct Active {
    source: WaitSource,
    owner: u64,
}

#[derive(Default)]
struct Slot {
    generation: u64,
    active: Option<Active>,
}

#[derive(Clone, Copy, Default)]
struct Handle {
    event_key: usize,
    read: Option<Registration>,
    write: Option<Registration>,
}

#[derive(Default)]
struct Core {
    slots: Vec<Slot>,
    free: Vec<usize>,
    timers: BTreeMap<(u64, u64), Registration>,
    handles: HashMap<u64, Handle>,
    poll_keys: HashMap<usize, u64>,
    next_poll_key: usize,
    ready: VecDeque<ReadyNotification>,
}

impl Core {
    fn active(&self, registration: Registration) -> Option<Active> {
        let slot = self.slots.get(usize::try_from(registration.key).ok()?)?;
        (slot.generation == registration.generation)
            .then_some(slot.active)
            .flatten()
    }

    fn insert(&mut self, source: WaitSource, owner: u64) -> io::Result<Registration> {
        let index = self.free.last().copied().unwrap_or(self.slots.len());
        let generation = self
            .slots
            .get(index)
            .map_or(0, |slot| slot.generation)
            .checked_add(1)
            .ok_or_else(|| io::Error::other("wait registration generation exhausted"))?;
        let key = u64::try_from(index)
            .map_err(|_| io::Error::other("wait registration identities exhausted"))?;
        if index == self.slots.len() {
            self.slots.push(Slot::default());
        } else {
            self.free.pop();
        }
        self.slots[index] = Slot {
            generation,
            active: Some(Active { source, owner }),
        };
        Ok(Registration { key, generation })
    }

    fn remove(&mut self, registration: Registration) -> Active {
        let index = registration.key as usize;
        let active = self.slots[index].active.take().unwrap();
        self.free.push(index);
        active
    }

    fn finish(&mut self, registration: Registration, events: u32, os_error: i32) {
        let active = self.remove(registration);
        self.ready.push_back(ReadyNotification {
            registration,
            owner: active.owner,
            events,
            os_error,
        });
    }

    fn fire_timers(&mut self, now: u64) {
        while let Some((&(deadline, _), &registration)) = self.timers.first_key_value() {
            if deadline > now {
                break;
            }
            self.timers.pop_first();
            self.finish(registration, TIMER, 0);
        }
    }

    fn poll_key(&mut self) -> io::Result<usize> {
        let next = self
            .next_poll_key
            .checked_add(1)
            .filter(|key| *key != usize::MAX)
            .ok_or_else(|| io::Error::other("wait polling identities exhausted"))?;
        self.next_poll_key = next;
        Ok(next)
    }
}

pub(crate) struct Reactor {
    poller: Poller,
    core: Mutex<Core>,
    events: Mutex<Events>,
}

fn lock<T>(mutex: &Mutex<T>) -> io::Result<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other("wait reactor lock poisoned"))
}

pub(crate) fn now_ns() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn loom_rt_monotonic_ns() -> i64 {
    i64::try_from(now_ns()).unwrap_or_else(|_| super::fatal("monotonic clock exceeds Int range"))
}

impl Reactor {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            poller: Poller::new()?,
            core: Mutex::new(Core::default()),
            events: Mutex::new(Events::new()),
        })
    }

    /// Register a timer, socket/fd readiness, or externally supplied completion.
    ///
    /// # Safety
    /// A readiness handle is borrowed. It must stay open until its registration
    /// fires, is successfully cancelled, or this reactor is dropped. If read
    /// and write have separate registrations, both must be retired before close.
    pub(crate) unsafe fn register(
        &self,
        source: WaitSource,
        owner: u64,
    ) -> io::Result<Registration> {
        match source.kind {
            KIND_TIMER | KIND_COMPLETION if source.interests == 0 => {}
            KIND_READINESS
                if source.interests != 0 && source.interests & !(READABLE | WRITABLE) == 0 => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid wait source",
                ));
            }
        }
        let mut core = lock(&self.core)?;
        if source.kind == KIND_READINESS {
            let handle = core
                .handles
                .get(&source.handle)
                .copied()
                .unwrap_or_default();
            if (source.interests & READABLE != 0 && handle.read.is_some())
                || (source.interests & WRITABLE != 0 && handle.write.is_some())
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "overlapping readiness registration",
                ));
            }
        }
        // Fail before publishing a borrowed registration if waking fails. The
        // core lock keeps a waking waiter from observing the old timer deadline.
        self.poller.notify()?;
        let registration = core.insert(source, owner)?;
        if source.kind == KIND_READINESS {
            let mut handle = core
                .handles
                .get(&source.handle)
                .copied()
                .unwrap_or_default();
            if source.interests & READABLE != 0 {
                handle.read = Some(registration);
            }
            if source.interests & WRITABLE != 0 {
                handle.write = Some(registration);
            }
            if let Err(error) = self.update_handle(&mut core, source.handle, handle) {
                core.remove(registration);
                return Err(error);
            }
        } else if source.kind == KIND_TIMER {
            core.timers
                .insert((source.deadline_ns, registration.key), registration);
        }
        Ok(registration)
    }

    fn update_handle(&self, core: &mut Core, source: u64, mut next: Handle) -> io::Result<()> {
        let previous = core.handles.get(&source).copied();
        // SAFETY: A readiness registration retains the caller's borrowed handle
        // until deletion. Core serializes updates against completion/cancel.
        let native = unsafe { native_source(source)? };
        let updated = if next.read.is_none() && next.write.is_none() {
            self.poller.delete(native)
        } else {
            next.event_key = core.poll_key()?;
            let event = Event::new(next.event_key, next.read.is_some(), next.write.is_some())
                .with_interrupt();
            if previous.is_some() {
                self.poller.modify(native, event)
            } else {
                // SAFETY: register's caller retains native until fire/cancel/drop.
                unsafe { self.poller.add(&native, event) }
            }
        };
        if let Err(error) = updated {
            // epoll ADD is atomic; unsupported descriptors (EPERM) were never
            // registered and cannot be deleted from the poller either.
            #[cfg(target_os = "linux")]
            if previous.is_none() {
                return Err(error);
            }
            // OS interest updates are not transactional (including failed add).
            // Remove partial state while the handle is still borrowed, then
            // restore precisely the old registrations. Its old key still names
            // the same identities, never a replacement registration.
            if let Err(cleanup) = self.poller.delete(native) {
                if cleanup.kind() != io::ErrorKind::NotFound {
                    super::fatal("failed to clean up wait registration");
                }
            }
            if let Some(previous) = previous {
                let event = Event::new(
                    previous.event_key,
                    previous.read.is_some(),
                    previous.write.is_some(),
                )
                .with_interrupt();
                // SAFETY: The unchanged active registrations retain the handle.
                if unsafe { self.poller.add(&native, event) }.is_err() {
                    super::fatal("failed to restore wait registration");
                }
            }
            return Err(error);
        }
        if let Some(previous) = previous {
            core.poll_keys.remove(&previous.event_key);
        }
        if next.read.is_none() && next.write.is_none() {
            core.handles.remove(&source);
        } else {
            core.poll_keys.insert(next.event_key, source);
            core.handles.insert(source, next);
        }
        Ok(())
    }

    pub(crate) fn cancel(&self, registration: Registration) -> io::Result<bool> {
        let mut core = lock(&self.core)?;
        let Some(active) = core.active(registration) else {
            return Ok(false);
        };
        // As in register, a failed wake must not commit an unseen transition.
        self.poller.notify()?;
        if active.source.kind == KIND_READINESS {
            let mut handle = core.handles[&active.source.handle];
            if handle.read == Some(registration) {
                handle.read = None;
            }
            if handle.write == Some(registration) {
                handle.write = None;
            }
            self.update_handle(&mut core, active.source.handle, handle)?;
        } else if active.source.kind == KIND_TIMER {
            core.timers
                .remove(&(active.source.deadline_ns, registration.key));
        }
        core.remove(registration);
        Ok(true)
    }

    // Completion is committed before wakeup: even if notify reports an OS
    // error, this registration is terminal and its notification remains queued.
    pub(crate) fn notify_completion(
        &self,
        registration: Registration,
        events: u32,
        os_error: i32,
    ) -> io::Result<bool> {
        if events == 0 || events & !(READABLE | WRITABLE | COMPLETION | ERROR) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid completion events",
            ));
        }
        let mut core = lock(&self.core)?;
        let Some(active) = core.active(registration) else {
            return Ok(false);
        };
        if active.source.kind != KIND_COMPLETION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a completion registration",
            ));
        }
        core.finish(
            registration,
            events | if os_error != 0 { ERROR } else { 0 },
            os_error,
        );
        drop(core);
        self.poller.notify()?;
        Ok(true)
    }

    fn readiness(&self, core: &mut Core, event: Event) -> io::Result<()> {
        let Some(&source) = core.poll_keys.get(&event.key) else {
            return Ok(()); // A queued notification from an older interest set.
        };
        let mut handle = core.handles[&source];
        let events = if event.readable { READABLE } else { 0 }
            | if event.writable { WRITABLE } else { 0 }
            | if event.is_interrupt() || event.is_err() == Some(true) {
                ERROR
            } else {
                0
            };
        let mut completed = [None, None];
        if events & (READABLE | ERROR) != 0 {
            completed[0] = handle.read.take();
        }
        if events & (WRITABLE | ERROR) != 0 {
            completed[1] = handle.write.take();
        }
        // A registration interested in both directions completes once.
        if let Some(registration) = completed[0].or(completed[1]) {
            if handle.read == Some(registration) {
                handle.read = None;
            }
            if handle.write == Some(registration) {
                handle.write = None;
            }
        }
        // Retire/rearm the native interest before exposing completion. A caller
        // may close the source as soon as its final ready notification is read.
        self.update_handle(core, source, handle)?;
        for (index, registration) in completed.into_iter().enumerate() {
            if let Some(registration) = registration {
                if index != 0 && completed[0] == Some(registration) {
                    continue;
                }
                let interests = core.active(registration).unwrap().source.interests;
                core.finish(registration, events & (interests | ERROR), 0);
            }
        }
        Ok(())
    }

    // One owner consumes waits/ready notifications; workers may register,
    // cancel and publish concurrently. Multiple waiters have no timeout promise
    // while waiting for the shared Events buffer.
    pub(crate) fn wait(&self, timeout: Option<Duration>) -> io::Result<usize> {
        {
            let mut core = lock(&self.core)?;
            core.fire_timers(now_ns());
            if !core.ready.is_empty() {
                return Ok(core.ready.len());
            }
        }
        let deadline = timeout
            .map(|duration| {
                Instant::now().checked_add(duration).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "wait timeout is too large")
                })
            })
            .transpose()?;
        let mut events = lock(&self.events)?;
        loop {
            let wait_for = {
                let mut core = lock(&self.core)?;
                let now = now_ns();
                core.fire_timers(now);
                if !core.ready.is_empty() {
                    return Ok(core.ready.len());
                }
                let timer = core
                    .timers
                    .first_key_value()
                    .map(|(&(due, _), _)| Duration::from_nanos(due.saturating_sub(now)));
                let remaining = deadline.map(|due| due.saturating_duration_since(Instant::now()));
                match (remaining, timer) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (one, other) => one.or(other),
                }
            };
            events.clear();
            // Only the event buffer is locked. Registration, cancellation, and
            // worker completion remain available while the poller blocks.
            self.poller.wait(&mut events, wait_for)?;
            let mut core = lock(&self.core)?;
            for event in events.iter() {
                self.readiness(&mut core, event)?;
            }
            core.fire_timers(now_ns());
            if !core.ready.is_empty() {
                return Ok(core.ready.len());
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(0);
            }
            // A notification can introduce an earlier timer, remove a wait,
            // or be spurious; re-evaluate the actual deadline and ready queue.
        }
    }

    pub(crate) fn pop_ready(&self) -> Option<ReadyNotification> {
        self.core
            .lock()
            .expect("wait reactor lock poisoned")
            .ready
            .pop_front()
    }
}

impl Drop for Reactor {
    fn drop(&mut self) {
        let core = self
            .core
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        for &handle in core.handles.keys() {
            // SAFETY: The caller retains active handles until Reactor is dropped.
            if let Ok(source) = unsafe { native_source(handle) } {
                let _ = self.poller.delete(source);
            }
        }
    }
}

#[cfg(unix)]
unsafe fn native_source<'a>(handle: u64) -> io::Result<std::os::fd::BorrowedFd<'a>> {
    let handle = i32::try_from(handle)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid native descriptor"))?;
    // SAFETY: register's caller lends a valid descriptor for the active wait.
    Ok(unsafe { std::os::fd::BorrowedFd::borrow_raw(handle) })
}

#[cfg(windows)]
unsafe fn native_source<'a>(handle: u64) -> io::Result<std::os::windows::io::BorrowedSocket<'a>> {
    #[cfg(target_pointer_width = "32")]
    let handle = u32::try_from(handle)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid native socket"))?;
    // SAFETY: register's caller lends a valid socket for the active wait.
    Ok(unsafe { std::os::windows::io::BorrowedSocket::borrow_raw(handle) })
}
