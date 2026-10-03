//! Bounded Loom CPU workers. Unlike native I/O jobs, these attach to the shared
//! moving heap and invoke compiler-instrumented code with rooted handoff frames.

use crate::cleanup::OwnedFault;
use crate::shared_heap::{self, SharedSlot};
use crate::wait::{COMPLETION, Reactor, Registration};
use crate::worker_control::{Control, Exit};
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

pub(super) type Callback = unsafe extern "C-unwind" fn(*mut u8);

enum State {
    Queued(Arc<SharedSlot>),
    Running,
    Complete(Result<Arc<SharedSlot>, Exit>),
    Taken,
}

pub(super) struct Job {
    state: Mutex<State>,
    finished: Condvar,
    control: Control,
    callback: Callback,
    reactor: Arc<Reactor>,
    registration: Registration,
}

impl Job {
    fn run(&self) {
        let frame = {
            let mut state = self.state.lock().unwrap();
            if !matches!(*state, State::Queued(_)) {
                return;
            }
            let State::Queued(frame) = std::mem::replace(&mut *state, State::Running) else {
                unreachable!()
            };
            frame
        };
        // The slot is read only after attaching; queued frames can move before
        // a worker starts. Callback return means cleanup and mutator exit ended.
        let outcome = unsafe { frame.enter(|value| self.control.run(|| (self.callback)(value))) };
        let result = outcome.map(|()| frame);
        *self.state.lock().unwrap() = State::Complete(result);
        self.finished.notify_all();
        if self
            .reactor
            .notify_completion(self.registration, COMPLETION, 0)
            .is_err()
        {
            crate::fatal("failed to notify worker completion");
        }
    }

    pub(super) fn take(&self) -> Result<(), OwnedFault> {
        match std::mem::replace(&mut *self.state.lock().unwrap(), State::Taken) {
            State::Complete(Ok(_frame)) => Ok(()),
            State::Complete(Err(Exit::Fault { message, test_name })) => {
                Err(OwnedFault { message, test_name })
            }
            _ => crate::fatal("worker result is not complete"),
        }
    }

    pub(super) fn complete_before_cancel(&self) -> bool {
        self.control.cancel();
        // Parent roots remain visible while the worker collects or cleans up.
        shared_heap::park_native(|| {
            let mut state = self.state.lock().unwrap();
            while matches!(*state, State::Running) {
                state = self.finished.wait(state).unwrap();
            }
            if matches!(
                *state,
                State::Complete(Ok(_)) | State::Complete(Err(Exit::Fault { .. }))
            ) {
                return true;
            }
            // A queued job never acquired a mutator; cancellation releases its
            // input without waiting for an occupied pool thread.
            *state = State::Taken;
            false
        })
    }

    pub(super) fn cancel_and_drain(&self) -> Option<OwnedFault> {
        if self.complete_before_cancel() {
            match std::mem::replace(&mut *self.state.lock().unwrap(), State::Taken) {
                State::Complete(Err(Exit::Fault { message, test_name })) => {
                    Some(OwnedFault { message, test_name })
                }
                _ => None,
            }
        } else {
            None
        }
    }

    pub(super) fn complete(&self) -> bool {
        matches!(*self.state.lock().unwrap(), State::Complete(_))
    }
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Arc<Job>>,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

pub(super) struct Pool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    pub(super) fn new() -> io::Result<Self> {
        let mut pool = Self {
            shared: Arc::new(Shared::default()),
            threads: Vec::new(),
        };
        let count = thread::available_parallelism()
            .map_or(1, usize::from)
            .min(4);
        for _ in 0..count {
            let shared = Arc::clone(&pool.shared);
            #[cfg(not(windows))]
            let arguments = crate::PROCESS_ARGS.get();
            #[cfg(not(windows))]
            let arguments = (arguments.0, arguments.1 as usize);
            #[cfg(windows)]
            let arguments = crate::PROCESS_ARGS.with(|arguments| arguments.borrow().clone());
            pool.threads.push(
                thread::Builder::new()
                    .name("loom-cpu".into())
                    .spawn(move || {
                        #[cfg(not(windows))]
                        crate::PROCESS_ARGS
                            .set((arguments.0, arguments.1 as *const *const std::ffi::c_char));
                        #[cfg(windows)]
                        crate::PROCESS_ARGS.with(|values| *values.borrow_mut() = arguments);
                        loop {
                            let job = {
                                let mut queue = shared.queue.lock().unwrap();
                                while queue.jobs.is_empty() && !queue.closed {
                                    queue = shared.ready.wait(queue).unwrap();
                                }
                                if queue.closed {
                                    return;
                                }
                                queue.jobs.pop_front().unwrap()
                            };
                            job.run();
                        }
                    })?,
            );
        }
        Ok(pool)
    }

    pub(super) unsafe fn submit(
        &self,
        frame: *mut u8,
        callback: Callback,
        reactor: Arc<Reactor>,
        registration: Registration,
    ) -> Arc<Job> {
        let job = Arc::new(Job {
            state: Mutex::new(State::Queued(unsafe { shared_heap::publish(frame) })),
            finished: Condvar::new(),
            control: Control::default(),
            callback,
            reactor,
            registration,
        });
        self.shared
            .queue
            .lock()
            .unwrap()
            .jobs
            .push_back(Arc::clone(&job));
        self.shared.ready.notify_one();
        job
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shared.queue.lock().unwrap().closed = true;
        self.shared.ready.notify_all();
        for thread in self.threads.drain(..) {
            thread
                .join()
                .unwrap_or_else(|_| crate::fatal("panic in Loom worker"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wait::{KIND_COMPLETION, WaitSource};
    use crate::{loom_rt_collect, loom_rt_text_new, rooted, text_bytes};

    unsafe extern "C-unwind" fn inspect(frame: *mut u8) {
        rooted([frame], |slots| unsafe {
            loom_rt_collect();
            assert_eq!(text_bytes(*slots), b"queued");
        });
    }

    #[test]
    fn queued_slots_relocate_before_attachment_and_completion_wins_late_cancel() {
        shared_heap::with_shared(|_| unsafe {
            let reactor = Arc::new(Reactor::new().unwrap());
            let registration = reactor
                .register(
                    WaitSource {
                        kind: KIND_COMPLETION,
                        interests: 0,
                        handle: 0,
                        deadline_ns: 0,
                    },
                    1,
                )
                .unwrap();
            // The slot, not a parent/native stack pointer, is the only root.
            let frame = shared_heap::publish(loom_rt_text_new(b"queued".as_ptr(), 6));
            let job = Arc::new(Job {
                state: Mutex::new(State::Queued(frame)),
                finished: Condvar::new(),
                control: Control::default(),
                callback: inspect,
                reactor,
                registration,
            });
            loom_rt_collect();
            let worker = {
                let job = Arc::clone(&job);
                thread::spawn(move || job.run())
            };
            shared_heap::park_native(|| worker.join().unwrap());
            loom_rt_collect();
            assert!(job.complete_before_cancel());
            job.take().unwrap();
            loom_rt_collect();
            assert!(!job.complete_before_cancel());
        });
    }
}
