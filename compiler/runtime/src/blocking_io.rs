//! Bounded native file workers. Jobs own only File/Vec values, never GC pointers.
//! Queued cancellation drops inputs immediately; running cancellation drains the
//! OS call before resource cleanup. Completion only publishes a wait identity.

use super::wait::{COMPLETION, Reactor, Registration};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

pub(super) enum Operation {
    Read(File, usize),
    Write(File, Vec<u8>),
    Failed,
    #[cfg(test)]
    Test(Box<dyn FnOnce() -> Outcome + Send>),
}

pub(super) struct Outcome {
    pub count: i64,
    pub bytes: Vec<u8>,
}

impl Operation {
    fn run(self) -> Outcome {
        let mut bytes = Vec::new();
        let count = match self {
            Self::Read(mut file, limit) => {
                bytes.resize(limit, 0);
                match file.read(&mut bytes) {
                    Ok(count) => {
                        bytes.truncate(count);
                        count as i64
                    }
                    Err(_) => {
                        bytes.clear();
                        -1
                    }
                }
            }
            Self::Write(mut file, input) => file.write(&input).map_or(-1, |count| count as i64),
            Self::Failed => -1,
            #[cfg(test)]
            Self::Test(run) => return run(),
        };
        // File and write input have been dropped before completion is published.
        Outcome { count, bytes }
    }
}

enum State {
    Queued(Operation),
    Running,
    Complete(Outcome),
    Taken,
}

pub(super) struct Job {
    state: Mutex<State>,
    finished: Condvar,
    reactor: Arc<Reactor>,
    registration: Registration,
}

impl Job {
    fn run(&self) {
        let operation = {
            let mut state = self.state.lock().unwrap();
            if !matches!(*state, State::Queued(_)) {
                return;
            }
            let State::Queued(operation) = std::mem::replace(&mut *state, State::Running) else {
                unreachable!()
            };
            operation
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation.run()))
            .unwrap_or_else(|_| super::fatal("panic in native file worker"));
        *self.state.lock().unwrap() = State::Complete(outcome);
        self.finished.notify_all();
        // A cancelled registration simply discards this identity. A failed OS
        // wake cannot be recovered by silently leaving the owner asleep.
        if self
            .reactor
            .notify_completion(self.registration, COMPLETION, 0)
            .is_err()
        {
            super::fatal("failed to notify file completion");
        }
    }

    pub(super) fn take(&self) -> Outcome {
        let mut state = self.state.lock().unwrap();
        match std::mem::replace(&mut *state, State::Taken) {
            State::Complete(outcome) => outcome,
            _ => super::fatal("file result is not complete"),
        }
    }

    pub(super) fn cancel_and_drain(&self) {
        let mut state = self.state.lock().unwrap();
        while matches!(*state, State::Running) {
            state = self.finished.wait(state).unwrap();
        }
        // Removes queued inputs or completed output before parent cleanup.
        *state = State::Taken;
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
    pub(super) fn new(count: usize) -> io::Result<Self> {
        assert!(count > 0);
        let mut pool = Self {
            shared: Arc::new(Shared::default()),
            threads: Vec::new(),
        };
        for _ in 0..count {
            let shared = pool.shared.clone();
            pool.threads
                .push(
                    thread::Builder::new()
                        .name("loom-file-io".into())
                        .spawn(move || {
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

    pub(super) fn submit(
        &self,
        operation: Operation,
        reactor: Arc<Reactor>,
        registration: Registration,
    ) -> Arc<Job> {
        let job = Arc::new(Job {
            state: Mutex::new(State::Queued(operation)),
            finished: Condvar::new(),
            reactor,
            registration,
        });
        self.shared
            .queue
            .lock()
            .unwrap()
            .jobs
            .push_back(job.clone());
        self.shared.ready.notify_one();
        job
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        {
            let mut queue = self.shared.queue.lock().unwrap();
            queue.closed = true;
            for job in queue.jobs.drain(..) {
                job.cancel_and_drain();
            }
        }
        self.shared.ready.notify_all();
        for worker in self.threads.drain(..) {
            if worker.join().is_err() {
                super::fatal("panic joining native file worker");
            }
        }
    }
}

#[cfg(test)]
#[path = "blocking_io_tests.rs"]
mod tests;
