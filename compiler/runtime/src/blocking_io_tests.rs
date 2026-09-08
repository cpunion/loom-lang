use super::*;
use crate::wait::{KIND_COMPLETION, WaitSource};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn registration(reactor: &Reactor, owner: u64) -> Registration {
    unsafe {
        reactor
            .register(
                WaitSource {
                    kind: KIND_COMPLETION,
                    interests: 0,
                    handle: 0,
                    deadline_ns: 0,
                },
                owner,
            )
            .unwrap()
    }
}

fn finish(reactor: &Reactor, registration: Registration, job: &Job) -> Outcome {
    let limit = Instant::now() + Duration::from_secs(5);
    loop {
        reactor.wait(Some(Duration::from_millis(100))).unwrap();
        if let Some(ready) = reactor.pop_ready() {
            assert_eq!(ready.registration, registration);
            return job.take();
        }
        assert!(
            Instant::now() < limit,
            "file completion did not wake the owner"
        );
    }
}

#[test]
fn workers_keep_native_files_and_binary_buffers_until_completion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("文件-🙂.bin");
    let payload = b"first\r\n\0\xfflast";
    let reactor = Arc::new(Reactor::new().unwrap());
    let pool = Pool::new(2).unwrap();
    let opened = registration(&reactor, 1);
    let job = pool.submit(
        Operation::Open(path.to_str().unwrap().to_owned(), true),
        reactor.clone(),
        opened,
    );
    let original = finish(&reactor, opened, &job).file.unwrap();
    let file = original.try_clone().unwrap();
    let registration1 = registration(&reactor, 1);
    let job = pool.submit(
        Operation::Write(file, payload.to_vec()),
        reactor.clone(),
        registration1,
    );
    drop(original);
    assert_eq!(
        finish(&reactor, registration1, &job).count,
        payload.len() as i64
    );
    assert_eq!(std::fs::read(&path).unwrap(), payload);
    let original = File::open(&path).unwrap();
    let mut copied = Vec::new();
    loop {
        let token = registration(&reactor, 2);
        let job = pool.submit(
            Operation::Read(original.try_clone().unwrap(), 3),
            reactor.clone(),
            token,
        );
        let chunk = finish(&reactor, token, &job);
        assert!(chunk.count >= 0);
        if chunk.count == 0 {
            break;
        }
        copied.extend_from_slice(&chunk.bytes);
    }
    let closed = registration(&reactor, 2);
    let job = pool.submit(Operation::Close(original), reactor.clone(), closed);
    assert_eq!(finish(&reactor, closed, &job).count, 0);
    assert_eq!(copied, payload);
    let token = registration(&reactor, 3);
    let job = pool.submit(Operation::Failed, reactor.clone(), token);
    assert_eq!(finish(&reactor, token, &job).count, -1);
}

#[test]
fn cancellation_discards_an_open_file_before_result_extraction() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unclaimed");
    let reactor = Arc::new(Reactor::new().unwrap());
    let pool = Pool::new(1).unwrap();
    let token = registration(&reactor, 1);
    let job = pool.submit(
        Operation::Open(path.to_str().unwrap().to_owned(), true),
        reactor.clone(),
        token,
    );
    // Wait on the job without extracting its result or delivering the wake.
    let state = job.state.lock().unwrap();
    let (state, timeout) = job
        .finished
        .wait_timeout_while(state, Duration::from_secs(5), |state| {
            !matches!(state, State::Complete(_))
        })
        .unwrap();
    assert!(!timeout.timed_out());
    assert!(matches!(&*state, State::Complete(outcome) if outcome.file.is_some()));
    drop(state);
    let cancelled = reactor.cancel(token).unwrap();
    job.cancel_and_drain();
    // Replacing Complete drops its owned File, even while the worker retains
    // the Job to finish publishing its now-cancelled notification.
    assert!(matches!(*job.state.lock().unwrap(), State::Taken));
    drop(pool);
    // Completion may already be queued; cancellation then returns false.
    // The scheduler rejects this old identity after removing the Task's wait.
    assert_eq!(
        reactor.pop_ready().map(|ready| ready.registration),
        if cancelled { None } else { Some(token) }
    );
    assert!(reactor.pop_ready().is_none());
}

struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn bounded_workers_drop_queued_inputs_and_drain_running_cancellation() {
    let reactor = Arc::new(Reactor::new().unwrap());
    let pool = Pool::new(1).unwrap();
    let (started, running) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let token = registration(&reactor, 1);
    let first = pool.submit(
        Operation::Test(Box::new(move || {
            started.send(()).unwrap();
            gate.recv().unwrap();
            Outcome {
                count: 7,
                bytes: vec![1, 2],
                file: None,
            }
        })),
        reactor.clone(),
        token,
    );
    running.recv_timeout(Duration::from_secs(5)).unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let input = Dropped(dropped.clone());
    let second_token = registration(&reactor, 2);
    let second = pool.submit(
        Operation::Test(Box::new(move || {
            drop(input);
            panic!("cancelled queued operation must not run");
        })),
        reactor.clone(),
        second_token,
    );
    reactor.cancel(second_token).unwrap();
    second.cancel_and_drain();
    assert!(dropped.load(Ordering::SeqCst));

    reactor.cancel(token).unwrap();
    let (done, drained) = mpsc::channel();
    let cancellation = thread::spawn(move || {
        first.cancel_and_drain();
        done.send(()).unwrap();
    });
    assert!(drained.recv_timeout(Duration::from_millis(20)).is_err());
    release.send(()).unwrap();
    drained.recv_timeout(Duration::from_secs(5)).unwrap();
    cancellation.join().unwrap();
    drop(pool);
    assert!(reactor.pop_ready().is_none());
}
