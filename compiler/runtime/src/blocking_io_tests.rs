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
    let fd = crate::file_io::open(path.to_str().unwrap(), true);
    let file = crate::file_io::duplicate(fd).unwrap();
    let registration1 = registration(&reactor, 1);
    let job = pool.submit(
        Operation::Write(file, payload.to_vec()),
        reactor.clone(),
        registration1,
    );
    assert_eq!(crate::file_io::close(fd), 0);
    assert_eq!(
        finish(&reactor, registration1, &job).count,
        payload.len() as i64
    );
    assert_eq!(std::fs::read(&path).unwrap(), payload);
    let fd = crate::file_io::open(path.to_str().unwrap(), false);
    let mut copied = Vec::new();
    loop {
        let token = registration(&reactor, 2);
        let job = pool.submit(
            Operation::Read(crate::file_io::duplicate(fd).unwrap(), 3),
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
    assert_eq!(crate::file_io::close(fd), 0);
    assert_eq!(copied, payload);
    assert!(crate::file_io::duplicate(-1).is_err());
    let token = registration(&reactor, 3);
    let job = pool.submit(Operation::Failed, reactor.clone(), token);
    assert_eq!(finish(&reactor, token, &job).count, -1);
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
