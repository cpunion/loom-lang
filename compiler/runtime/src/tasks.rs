//! Private, single-owner hot tasks. Only async entries install this scheduler.
//! A compiler-defined moving frame stores its typed result at byte zero; task
//! identities and static creation labels contain no managed pointers. Resumes
//! root/reload their frame across allocation and return 0 (done) or 1 (waiting).
//! This initial suspension ABI permits no live lexical cleanup registrations:
//! cancelling a queued/waiting subtree releases its frames, not user callbacks.

use super::cleanup::{OwnedFault, catch_fault, raise_owned};
use super::frame_roots::{FrameRootId, FrameRoots, with_frame_roots};
use super::{fatal, fault};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ptr;

type Resume = unsafe extern "C-unwind" fn(*mut u8) -> i64;
type Constructor = unsafe extern "C-unwind" fn() -> u64;

enum State {
    Queued,
    Running,
    Waiting(u64),
    Completed,
    Faulted(OwnedFault),
}

struct Task {
    frame: FrameRootId,
    resume: Resume,
    parent: Option<u64>,
    children: BTreeSet<u64>,
    waiter: Option<u64>,
    state: State,
    creation: (*const u8, usize),
}

#[derive(Default)]
struct Core {
    tasks: HashMap<u64, Task>,
    ready: VecDeque<u64>,
    current: Option<u64>,
    root: Option<u64>,
    next: u64,
}

struct Owner {
    roots: *const FrameRoots,
    core: RefCell<Core>,
}

thread_local! {
    static OWNER: Cell<*const Owner> = const { Cell::new(ptr::null()) };
}

impl Owner {
    fn roots(&self) -> &FrameRoots {
        // SAFETY: The outer with_frame_roots scope outlives this native owner.
        unsafe { &*self.roots }
    }

    // Cancellation has no callback in this slice. Visit only live descendants,
    // children before parents; removed queued identities are skipped on dequeue.
    fn cancel_tree(&self, core: &mut Core, id: u64) {
        let mut pending = vec![(id, false)];
        while let Some((id, expanded)) = pending.pop() {
            if !expanded {
                let task = core.tasks.get(&id).expect("live cancelled task");
                pending.push((id, true));
                pending.extend(task.children.iter().map(|child| (*child, false)));
            } else {
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

    fn cancel_descendants(&self, core: &mut Core, id: u64) {
        while let Some(child) = core.tasks[&id].children.first().copied() {
            self.cancel_tree(core, child);
        }
    }

    fn validate_return(&self, id: u64, status: i64) -> Result<(), &'static str> {
        let core = self.core.borrow();
        let task = &core.tasks[&id];
        match (status, &task.state) {
            (0, State::Running) if task.children.is_empty() => Ok(()),
            (0, _) => Err("task completed with outstanding children or wait"),
            (1, State::Waiting(_)) => Ok(()),
            _ => Err("invalid task resume state"),
        }
    }

    fn finish_resume(&self, id: u64, outcome: Result<i64, OwnedFault>) {
        let mut core = self.core.borrow_mut();
        core.current = None;
        let terminal = match outcome {
            Ok(0) => {
                core.tasks.get_mut(&id).unwrap().state = State::Completed;
                true
            }
            Ok(1) => false,
            Ok(_) => fatal("unchecked task resume state"),
            Err(mut failure) => {
                self.cancel_descendants(&mut core, id);
                let task = core.tasks.get_mut(&id).unwrap();
                if task.creation.1 != 0 {
                    failure.message.push(b'\n');
                    // SAFETY: task_create retains only compiler-owned static
                    // labels, never moving Text or a native stack byte buffer.
                    failure.message.extend_from_slice(unsafe {
                        std::slice::from_raw_parts(task.creation.0, task.creation.1)
                    });
                }
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
            let next = {
                let mut core = self.core.borrow_mut();
                if matches!(
                    core.tasks[&root].state,
                    State::Completed | State::Faulted(_)
                ) {
                    let task = core.tasks.remove(&root).unwrap();
                    assert!(task.children.is_empty() && core.tasks.is_empty());
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
                        fatal("task wait has no runnable dependency");
                    };
                    if core.tasks.contains_key(&id) {
                        break id;
                    }
                };
                let task = core.tasks.get_mut(&id).unwrap();
                if !matches!(task.state, State::Queued) {
                    fatal("invalid ready task state");
                }
                task.state = State::Running;
                let next = (id, task.frame, task.resume);
                core.current = Some(id);
                next
            };
            let (id, frame, resume) = next;
            // Reload after every previous callback/collection. Neither Core nor
            // the root store remains borrowed while generated code executes.
            let frame = self.roots().get(frame).expect("rooted task frame");
            // SAFETY: Generated resumes obey catch_fault's stack/unwind contract
            // and leave no lexical cleanup live when returning Pending.
            let outcome = unsafe {
                catch_fault(|| {
                    let status = resume(frame);
                    self.validate_return(id, status)
                        .unwrap_or_else(|message| fault(message));
                    status
                })
            };
            self.finish_resume(id, outcome);
        }
    }
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
                waiter: None,
                state: State::Queued,
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
        if !matches!(core.tasks[&parent].state, State::Running)
            && !matches!(core.tasks[&parent].state, State::Waiting(id) if id == child)
        {
            return Err("task already awaits another child");
        }
        let task = core.tasks.get_mut(&child).unwrap();
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
            State::Completed => Ok(Ok(owner
                .roots()
                .get(task.frame)
                .expect("rooted task result"))),
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
        let task = core.tasks.remove(&child).unwrap();
        assert!(task.children.is_empty());
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
                let mut core = owner.core.borrow_mut();
                if let Some(root) = core.root.take() {
                    owner.cancel_tree(&mut core, root);
                }
                core.ready.clear();
                Err(failure)
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
