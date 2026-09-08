//! Private, single-owner hot tasks. Only async entries install this scheduler.
//! A compiler-defined moving frame stores its typed result at byte zero; task
//! identities and static creation labels contain no managed pointers. Resumes
//! root/reload their frame across allocation and return 0 (done) or 1 (waiting).
//! Async cleanup captures are authoritative frame fields, never suspended stack
//! slots. Cancellation retires waits and drains children before parent cleanup.
//! The reactor is created only on the first external wait.

use super::blocking_io::{Job, Operation, Outcome};
use super::cleanup::{OwnedFault, catch_fault, catch_fault_before_drain, raise_owned};
use super::frame_roots::{FrameRootId, FrameRoots, with_frame_roots};
use super::wait::{
    KIND_COMPLETION, KIND_TIMER, Reactor, ReadyNotification, Registration, WaitSource,
};
use super::{fatal, fault};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::{io, ptr, time::Duration};

type Resume = unsafe extern "C-unwind" fn(*mut u8) -> i64;
type Constructor = unsafe extern "C-unwind" fn() -> u64;
type CleanupCallback = unsafe extern "C-unwind" fn(*mut u8);

#[path = "tasks_file_io.rs"]
mod file_io;

#[derive(Clone, Copy)]
struct TaskCleanup {
    site: i64,
    callback: CleanupCallback,
}

enum State {
    Queued,
    Running,
    Waiting(u64),
    ExternalWaiting,
    Completed,
    Faulted(OwnedFault),
}

#[derive(Clone, Copy)]
struct ExternalWait {
    source: WaitSource,
    registration: Registration,
    ready: Option<ReadyNotification>,
}

struct Task {
    frame: FrameRootId,
    resume: Resume,
    parent: Option<u64>,
    children: BTreeSet<u64>,
    // Returned Tasks stay in the existing child set until extraction. Retain
    // the count afterward to reject a second extraction without another set.
    returned: usize,
    returned_to_parent: bool,
    cleanups: Vec<TaskCleanup>,
    waiter: Option<u64>,
    state: State,
    external: Option<ExternalWait>,
    operation: Option<Arc<Job>>,
    creation: (*const u8, usize),
}

#[derive(Default)]
struct Core {
    tasks: HashMap<u64, Task>,
    ready: VecDeque<u64>,
    current: Option<u64>,
    root: Option<u64>,
    next: u64,
    pending: usize,
}

struct Owner {
    roots: *const FrameRoots,
    core: RefCell<Core>,
    reactor: OnceCell<Arc<Reactor>>,
    workers: OnceCell<file_io::Files>,
}

thread_local! {
    static OWNER: Cell<*const Owner> = const { Cell::new(ptr::null()) };
}

impl Owner {
    fn roots(&self) -> &FrameRoots {
        // SAFETY: The outer with_frame_roots scope outlives this native owner.
        unsafe { &*self.roots }
    }

    fn reactor(&self) -> io::Result<&Arc<Reactor>> {
        if self.reactor.get().is_none() && self.reactor.set(Arc::new(Reactor::new()?)).is_err() {
            fatal("task reactor initialized twice");
        }
        Ok(self.reactor.get().unwrap())
    }

    fn workers(&self) -> io::Result<&file_io::Files> {
        if self.workers.get().is_none() {
            let count = std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(4);
            if self.workers.set(file_io::Files::new(count)?).is_err() {
                fatal("file workers initialized twice");
            }
        }
        Ok(self.workers.get().unwrap())
    }

    fn cancel_operation(&self, id: u64) {
        let operation = self
            .core
            .borrow_mut()
            .tasks
            .get_mut(&id)
            .unwrap()
            .operation
            .take();
        if let Some(operation) = operation {
            operation.cancel_and_drain();
        }
    }

    fn cancel_wait(&self, core: &mut Core, id: u64) {
        if let Some(wait) = core.tasks.get_mut(&id).unwrap().external.take() {
            if wait.ready.is_none() {
                // A false result means completion is already queued. Either
                // way, its old identity cannot wake a removed/replaced wait.
                if self
                    .reactor
                    .get()
                    .unwrap()
                    .cancel(wait.registration)
                    .is_err()
                {
                    fatal("failed to cancel task wait");
                }
                core.pending -= 1;
            }
        }
    }

    fn poll(&self, blocking: bool) -> io::Result<()> {
        if self.core.borrow().pending == 0 {
            return Ok(());
        }
        let reactor = self.reactor.get().expect("registered task wait");
        // No task/root/heap borrow crosses this OS wait. Workers can only
        // publish identities; the owner alone queues and resumes task frames.
        reactor.wait(if blocking { None } else { Some(Duration::ZERO) })?;
        while let Some(notification) = reactor.pop_ready() {
            let mut core = self.core.borrow_mut();
            let Some(task) = core.tasks.get_mut(&notification.owner) else {
                continue;
            };
            let Some(wait) = task.external.as_mut() else {
                continue;
            };
            if !matches!(task.state, State::ExternalWaiting)
                || wait.registration != notification.registration
                || wait.ready.is_some()
            {
                continue;
            }
            wait.ready = Some(notification);
            task.state = State::Queued;
            core.pending -= 1;
            core.ready.push_back(notification.owner);
        }
        Ok(())
    }

    fn drain_cleanups(&self, id: u64, first: &mut Option<OwnedFault>) {
        loop {
            let next = {
                let mut core = self.core.borrow_mut();
                let task = core.tasks.get_mut(&id).expect("live task cleanup");
                task.cleanups
                    .pop()
                    .map(|cleanup| (cleanup, task.frame, task.creation))
            };
            let Some((cleanup, frame, creation)) = next else {
                return;
            };
            // Pop before invocation and reload after every prior cleanup/GC.
            // No task, heap or root-store borrow crosses generated code.
            let frame = self.roots().get(frame).expect("rooted task cleanup frame");
            let previous = self.core.borrow_mut().current.take();
            // SAFETY: Generated cleanup roots this frame parameter, cannot
            // suspend or use Task operations, and obeys the native fault ABI.
            let outcome = unsafe { catch_fault(|| (cleanup.callback)(frame)) };
            self.core.borrow_mut().current = previous;
            if let Err(mut failure) = outcome {
                if first.is_none() {
                    append_creation(&mut failure, creation);
                    *first = Some(failure);
                }
            }
        }
    }

    // Visit only live descendants, children before parents. Native traversal
    // contains IDs only; callbacks run with every Core borrow released.
    fn cancel_tree(&self, id: u64, first: &mut Option<OwnedFault>) {
        let mut pending = vec![(id, false)];
        while let Some((id, expanded)) = pending.pop() {
            if !expanded {
                let core = self.core.borrow();
                let task = core.tasks.get(&id).expect("live cancelled task");
                pending.push((id, true));
                pending.extend(task.children.iter().map(|child| (*child, false)));
            } else {
                self.cancel_wait(&mut self.core.borrow_mut(), id);
                self.cancel_operation(id);
                self.drain_cleanups(id, first);
                let mut core = self.core.borrow_mut();
                let task = core.tasks.remove(&id).expect("live cancelled task");
                if let Some(parent) = task.parent {
                    core.tasks
                        .get_mut(&parent)
                        .expect("live task parent")
                        .children
                        .remove(&id);
                }
                assert!(self.roots().remove(task.frame));
            }
        }
    }

    fn cancel_descendants(&self, id: u64, first: &mut Option<OwnedFault>) {
        loop {
            let child = self.core.borrow().tasks[&id].children.first().copied();
            let Some(child) = child else {
                return;
            };
            self.cancel_tree(child, first);
        }
    }

    fn validate_return(&self, id: u64, status: i64) -> Result<(), &'static str> {
        let core = self.core.borrow();
        let task = &core.tasks[&id];
        let children_done = task.children.len() == task.returned;
        match (status, &task.state) {
            (0, State::Running)
                if children_done
                    && task.external.is_none()
                    && task.operation.is_none()
                    && task.cleanups.is_empty() =>
            {
                Ok(())
            }
            (0, _) => Err("task completed with outstanding children or wait"),
            (1, State::Waiting(_)) if task.external.is_none() && task.returned == 0 => Ok(()),
            (1, State::ExternalWaiting)
                if task.returned == 0 && task.external.is_some_and(|wait| wait.ready.is_none()) =>
            {
                Ok(())
            }
            _ => Err("invalid task resume state"),
        }
    }

    fn finish_resume(&self, id: u64, outcome: Result<i64, OwnedFault>) {
        self.core.borrow_mut().current = None;
        let outcome = match outcome {
            Err(failure) => {
                // The pre-drain hook already retired this wait and all children;
                // live synchronous helper cleanup ran before catch returned.
                // Async captures remain authoritative in the rooted frame.
                let mut first = Some(failure);
                self.drain_cleanups(id, &mut first);
                Err(first.unwrap())
            }
            other => other,
        };
        let mut core = self.core.borrow_mut();
        let terminal = match outcome {
            Ok(0) => {
                core.tasks.get_mut(&id).unwrap().state = State::Completed;
                true
            }
            Ok(1) => false,
            Ok(_) => fatal("unchecked task resume state"),
            Err(mut failure) => {
                let task = core.tasks.get_mut(&id).unwrap();
                append_creation(&mut failure, task.creation);
                task.state = State::Faulted(failure);
                true
            }
        };
        if terminal {
            let task = &core.tasks[&id];
            let priority = matches!(task.state, State::Faulted(_));
            if let Some(parent) = task.waiter {
                let parent_task = core.tasks.get_mut(&parent).expect("live task waiter");
                if !matches!(parent_task.state, State::Waiting(child) if child == id) {
                    fatal("invalid task waiter state");
                }
                parent_task.state = State::Queued;
                // Observe an awaited failure before starting more queued work.
                // This is not a source-level sibling order/fairness guarantee.
                if priority {
                    core.ready.push_front(parent);
                } else {
                    core.ready.push_back(parent);
                }
            }
        }
    }

    unsafe fn drive(&self, root: u64) -> Result<(), OwnedFault> {
        loop {
            if let Err(error) = self.poll(false) {
                return Err(self.wait_failure(root, error));
            }
            let next = {
                let mut core = self.core.borrow_mut();
                if matches!(
                    core.tasks[&root].state,
                    State::Completed | State::Faulted(_)
                ) {
                    let task = core.tasks.remove(&root).unwrap();
                    assert!(task.children.is_empty() && core.tasks.is_empty() && core.pending == 0);
                    assert!(self.roots().remove(task.frame));
                    core.root = None;
                    core.ready.clear();
                    return match task.state {
                        State::Completed => Ok(()),
                        State::Faulted(failure) => Err(failure),
                        _ => unreachable!(),
                    };
                }
                let id = loop {
                    let Some(id) = core.ready.pop_front() else {
                        break None;
                    };
                    if core.tasks.contains_key(&id) {
                        break Some(id);
                    }
                };
                if let Some(id) = id {
                    let task = core.tasks.get_mut(&id).unwrap();
                    if !matches!(task.state, State::Queued) {
                        fatal("invalid ready task state");
                    }
                    task.state = State::Running;
                    let next = (id, task.frame, task.resume);
                    core.current = Some(id);
                    Some(next)
                } else {
                    if core.pending == 0 {
                        fatal("task wait has no runnable dependency");
                    }
                    None
                }
            };
            let Some((id, frame, resume)) = next else {
                if let Err(error) = self.poll(true) {
                    return Err(self.wait_failure(root, error));
                }
                continue;
            };
            // Reload after every previous callback/collection. Neither Core nor
            // the root store remains borrowed while generated code executes.
            let frame = self.roots().get(frame).expect("rooted task frame");
            // SAFETY: Generated resumes keep their own cleanup captures in the
            // rooted frame. Native helper cleanup balances before Pending; on a
            // fault, the hook drains children before live helper stacks unwind.
            let outcome = unsafe {
                catch_fault_before_drain(
                    || {
                        let status = resume(frame);
                        self.validate_return(id, status)
                            .unwrap_or_else(|message| fault(message));
                        status
                    },
                    before_resume_fault,
                    ptr::from_ref(self).cast_mut().cast(),
                )
            };
            self.finish_resume(id, outcome);
        }
    }

    fn wait_failure(&self, root: u64, error: io::Error) -> OwnedFault {
        // Capture the diagnostic with the existing test/fault context, but do
        // not propagate it through an installed owner or its outer root scope.
        // SAFETY: No activation, cleanup, or heap borrow is live here.
        let failure = unsafe { catch_fault(|| wait_fault(error)) }.unwrap_err();
        let mut first = Some(failure);
        self.cancel_tree(root, &mut first);
        let mut core = self.core.borrow_mut();
        core.root = None;
        core.ready.clear();
        first.unwrap()
    }
}

unsafe fn before_resume_fault(data: *mut u8) {
    // SAFETY: drive retains this fixed owner through the entire resume/catch.
    let owner = unsafe { &*data.cast::<Owner>() };
    let id = owner
        .core
        .borrow()
        .current
        .expect("faulting task activation");
    // The parent diagnostic is already owned by catch_fault. Secondary cleanup
    // faults cannot replace it, but every descendant still must drain.
    let mut secondary = None;
    owner.cancel_descendants(id, &mut secondary);
    owner.cancel_wait(&mut owner.core.borrow_mut(), id);
    owner.cancel_operation(id);
}

fn append_creation(failure: &mut OwnedFault, creation: (*const u8, usize)) {
    if creation.1 != 0 {
        failure.message.push(b'\n');
        // SAFETY: task_create accepts executable-lifetime native labels only.
        failure
            .message
            .extend_from_slice(unsafe { std::slice::from_raw_parts(creation.0, creation.1) });
    }
}

fn wait_fault(error: io::Error) -> ! {
    if error.kind() == io::ErrorKind::OutOfMemory {
        fatal("out of memory");
    }
    fault(&format!("task wait failed: {error}"));
}

// Readiness adapters must retain the native handle until completion/cancel;
// this internal operation does not acquire ownership of any source resource.
unsafe fn wait_source(source: WaitSource) -> Option<ReadyNotification> {
    let outcome = edit(|owner, core| {
        let id = core.current.ok_or("task wait outside a resume")?;
        let task = core.tasks.get_mut(&id).unwrap();
        if !matches!(task.state, State::Running | State::ExternalWaiting) {
            return Err("task already awaits a child");
        }
        if let Some(wait) = task.external {
            let previous = wait.source;
            if (
                previous.kind,
                previous.interests,
                previous.handle,
                previous.deadline_ns,
            ) != (
                source.kind,
                source.interests,
                source.handle,
                source.deadline_ns,
            ) {
                return Err("task already has another external wait");
            }
            if wait.ready.is_some() {
                task.external = None;
                task.state = State::Running;
            }
            return Ok(Ok(wait.ready));
        }
        let registration = owner.reactor().and_then(|reactor| {
            // SAFETY: The adapter retains a readiness handle through drain.
            unsafe { reactor.register(source, id) }
        });
        match registration {
            Ok(registration) => {
                task.external = Some(ExternalWait {
                    source,
                    registration,
                    ready: None,
                });
                task.state = State::ExternalWaiting;
                core.pending += 1;
                Ok(Ok(None))
            }
            Err(error) => Ok(Err(error)),
        }
    });
    // Reactor failures, like invalid task operations, must not unwind while
    // Core is borrowed: fault cleanup can allocate and inspect task state.
    outcome.unwrap_or_else(|error| wait_fault(error))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_wait_timer(deadline_ns: i64) -> i32 {
    if deadline_ns < 0 {
        fault("timer deadline must not be negative");
    }
    // SAFETY: A timer borrows no native resource.
    let ready = unsafe {
        wait_source(WaitSource {
            kind: KIND_TIMER,
            interests: 0,
            handle: 0,
            deadline_ns: deadline_ns as u64,
        })
    };
    i32::from(ready.is_some())
}

fn edit<R>(operation: impl FnOnce(&Owner, &mut Core) -> Result<R, &'static str>) -> R {
    let owner = OWNER.get();
    if owner.is_null() {
        fault("Task requires an async entry");
    }
    // SAFETY: task_run publishes this fixed owner only for its complete scope.
    let owner = unsafe { &*owner };
    let result = {
        let mut core = owner.core.borrow_mut();
        operation(owner, &mut core)
    };
    // Invalid source/private-ABI operations may drain allocating cleanup.
    // Release Core before raising the fault.
    result.unwrap_or_else(|message| fault(message))
}

fn parent(core: &Core, child: u64) -> Result<u64, &'static str> {
    let parent = core.current.ok_or("task operation outside a resume")?;
    let child = core.tasks.get(&child).ok_or("task already consumed")?;
    if child.parent != Some(parent) {
        return Err("task belongs to another parent");
    }
    Ok(parent)
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_cleanup_push(site: i64, callback: CleanupCallback) {
    edit(|_, core| {
        let id = core.current.ok_or("task cleanup outside a resume")?;
        let task = core.tasks.get_mut(&id).unwrap();
        if site < 0 || !matches!(task.state, State::Running) {
            return Err("invalid task cleanup registration");
        }
        task.cleanups.push(TaskCleanup { site, callback });
        Ok(())
    });
}

// Normal lexical exit pops before a generated direct call to callback(frame).
// Only fault/cancellation dispatches callbacks indirectly through this stack.
#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_cleanup_pop(site: i64) {
    edit(|_, core| {
        let id = core.current.ok_or("task cleanup outside a resume")?;
        let task = core.tasks.get_mut(&id).unwrap();
        if !matches!(task.state, State::Running)
            || task
                .cleanups
                .last()
                .is_none_or(|cleanup| cleanup.site != site)
        {
            return Err("invalid task cleanup registration order");
        }
        task.cleanups.pop();
        Ok(())
    });
}

// Callers validate the old parent and distinct, noncyclic destination before
// editing. IDs, ready entries, registrations and the entire child subtree stay
// unchanged; this transfers an obligation, not a frame or a running activation.
fn reparent(core: &mut Core, child: u64, previous: u64, next: u64) {
    assert!(
        core.tasks
            .get_mut(&previous)
            .unwrap()
            .children
            .remove(&child)
    );
    assert!(core.tasks.get_mut(&next).unwrap().children.insert(child));
    let task = core.tasks.get_mut(&child).unwrap();
    task.parent = Some(next);
    task.returned_to_parent = false;
}

/// Lowering calls this immediately after creating the queued callee, once for
/// each Task in its parameters. Current direct children are disjoint subtrees.
#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_adopt(callee: u64, child: u64) {
    edit(|_, core| {
        let current = parent(core, callee)?;
        parent(core, child)?;
        if !matches!(core.tasks[&current].state, State::Running)
            || core.tasks[&current].external.is_some()
        {
            return Err("task transfer requires a running caller");
        }
        if callee == child {
            return Err("task cannot adopt itself");
        }
        if !matches!(core.tasks[&callee].state, State::Queued)
            || core.tasks[&callee].waiter.is_some()
            || core.tasks[&child].waiter.is_some()
            || core.tasks[&child].returned_to_parent
        {
            return Err("task adoption requires a queued callee and unawaited child");
        }
        reparent(core, child, current, callee);
        Ok(())
    });
}

/// Mark one Task in the logical result, retaining it under this producer until
/// its actual consumer extracts the result. No allocation or user callback.
#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_return(child: u64) -> u64 {
    edit(|_, core| {
        let current = parent(core, child)?;
        let task = &core.tasks[&current];
        if task.parent.is_none() {
            return Err("async entry cannot return a Task");
        }
        if !matches!(task.state, State::Running) || task.external.is_some() {
            return Err("task return requires a running producer");
        }
        if core.tasks[&child].returned_to_parent || core.tasks[&child].waiter.is_some() {
            return Err("task return requires a distinct unawaited child");
        }
        core.tasks.get_mut(&child).unwrap().returned_to_parent = true;
        core.tasks.get_mut(&current).unwrap().returned += 1;
        Ok(child)
    })
}

/// The frame is an initialized GC allocation base; creation is a fully rendered
/// static native UTF-8 diagnostic valid for the executable lifetime (null only
/// if empty), not a moving Text pointer.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_task_create(
    frame: *mut u8,
    resume: Resume,
    creation: *const u8,
    creation_len: usize,
) -> u64 {
    edit(|owner, core| {
        if let Some(parent) = core.current {
            if !matches!(core.tasks[&parent].state, State::Running) {
                return Err("task created after a pending await");
            }
        } else if core.root.is_some() {
            return Err("async entry constructor must create one root task");
        }
        let id = core
            .next
            .checked_add(1)
            .unwrap_or_else(|| fatal("task identities exhausted"));
        // SAFETY: The caller roots construction across allocation; insert itself
        // never collects, so no snapshot gap exists before scheduler ownership.
        let frame = unsafe { owner.roots().insert(frame) };
        let parent = core.current;
        core.tasks.insert(
            id,
            Task {
                frame,
                resume,
                parent,
                children: BTreeSet::new(),
                returned: 0,
                returned_to_parent: false,
                cleanups: Vec::new(),
                waiter: None,
                state: State::Queued,
                external: None,
                operation: None,
                creation: (creation, creation_len),
            },
        );
        core.next = id;
        if let Some(parent) = parent {
            core.tasks.get_mut(&parent).unwrap().children.insert(id);
        } else {
            core.root = Some(id);
        }
        core.ready.push_back(id);
        Ok(id)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_await(child: u64) -> i32 {
    edit(|_, core| {
        let parent = parent(core, child)?;
        if core.tasks[&parent].external.is_some() {
            return Err("task already has an external wait");
        }
        if !matches!(core.tasks[&parent].state, State::Running)
            && !matches!(core.tasks[&parent].state, State::Waiting(id) if id == child)
        {
            return Err("task already awaits another child");
        }
        let task = core.tasks.get_mut(&child).unwrap();
        if task.returned_to_parent {
            return Err("cannot await a returned task before extraction");
        }
        if task.waiter.is_some_and(|waiter| waiter != parent) {
            return Err("task already has a waiter");
        }
        let ready = matches!(task.state, State::Completed | State::Faulted(_));
        task.waiter = Some(parent);
        if !ready {
            core.tasks.get_mut(&parent).unwrap().state = State::Waiting(child);
        }
        Ok(i32::from(ready))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_result(child: u64) -> *mut u8 {
    let result = edit(|owner, core| {
        let parent = parent(core, child)?;
        let task = &core.tasks[&child];
        if task.waiter != Some(parent) {
            return Err("task result requires await");
        }
        match &task.state {
            State::Completed => {
                let frame = task.frame;
                if task.returned != 0 {
                    if task.children.len() != task.returned {
                        return Err("task result already extracted");
                    }
                    while let Some(returned) = core.tasks[&child].children.first().copied() {
                        let result = &core.tasks[&returned];
                        if result.parent != Some(child)
                            || result.waiter.is_some()
                            || !result.returned_to_parent
                        {
                            return Err("task result is not an unawaited child");
                        }
                        reparent(core, returned, child, parent);
                    }
                }
                Ok(Ok(owner.roots().get(frame).expect("rooted task result")))
            }
            State::Faulted(failure) => Ok(Err(OwnedFault {
                message: failure.message.clone(),
                test_name: failure.test_name.clone(),
            })),
            _ => Err("task result is not ready"),
        }
    });
    match result {
        Ok(frame) => frame,
        Err(failure) => raise_owned(failure),
    }
}

/// The receiver roots its typed result snapshot before releasing this frame.
#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_release(child: u64) {
    edit(|owner, core| {
        let parent = parent(core, child)?;
        let task = &core.tasks[&child];
        if task.waiter != Some(parent) || !matches!(task.state, State::Completed) {
            return Err("task release requires a completed awaited result");
        }
        if !task.children.is_empty() {
            return Err("task release requires extracting its Task result");
        }
        let task = core.tasks.remove(&child).unwrap();
        core.tasks.get_mut(&parent).unwrap().children.remove(&child);
        assert!(owner.roots().remove(task.frame));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_task_run(constructor: Constructor) {
    if !OWNER.get().is_null() {
        fault("nested task executors are not supported");
    }
    let outcome = with_frame_roots(|roots| {
        let owner = Owner {
            roots: ptr::from_ref(roots),
            core: RefCell::new(Core::default()),
            reactor: OnceCell::new(),
            workers: OnceCell::new(),
        };
        OWNER.set(ptr::from_ref(&owner));
        // SAFETY: The owner remains rooted outside all fault boundaries. The
        // constructor publishes exactly one root and never runs its body inline.
        let root = unsafe {
            catch_fault(|| {
                let root = constructor();
                edit(|_, core| {
                    if core.root != Some(root) {
                        return Err("async entry constructor returned a different task");
                    }
                    Ok(root)
                })
            })
        };
        let outcome = match root {
            // SAFETY: Each resume runs under its own live-stack fault boundary.
            Ok(root) => unsafe { owner.drive(root) },
            Err(failure) => {
                let mut first = Some(failure);
                let root = owner.core.borrow_mut().root.take();
                if let Some(root) = root {
                    owner.cancel_tree(root, &mut first);
                }
                owner.core.borrow_mut().ready.clear();
                Err(first.unwrap())
            }
        };
        OWNER.set(ptr::null());
        outcome
    });
    // Reporting/propagation happens only after descendant drain, root-scope
    // exit and TLS restoration. Synchronous process-fault behavior is unchanged.
    if let Err(failure) = outcome {
        raise_owned(failure);
    }
}

#[cfg(test)]
#[path = "tasks_wait_tests.rs"]
mod wait_tests;

#[cfg(test)]
#[path = "tasks_transfer_tests.rs"]
mod transfer_tests;

#[cfg(test)]
#[path = "tasks_cleanup_tests.rs"]
mod cleanup_tests;
