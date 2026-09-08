use super::wait::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

fn ready(reactor: &Reactor, count: usize) -> Vec<ReadyNotification> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut notifications = Vec::new();
    loop {
        while let Some(notification) = reactor.pop_ready() {
            notifications.push(notification);
        }
        if notifications.len() >= count {
            assert_eq!(
                notifications.len(),
                count,
                "unexpected duplicate notification"
            );
            return notifications;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "wait notification deadline exhausted");
        reactor.wait(Some(remaining)).unwrap();
    }
}

fn completion(reactor: &Reactor, owner: u64) -> Registration {
    // Completion sources do not borrow a native handle.
    unsafe {
        reactor.register(
            WaitSource {
                kind: KIND_COMPLETION,
                interests: 0,
                handle: 0,
                deadline_ns: 0,
            },
            owner,
        )
    }
    .unwrap()
}

fn same_registration(actual: Registration, expected: Registration) {
    assert_eq!(actual.key, expected.key);
    assert_eq!(actual.generation, expected.generation);
}

#[test]
fn timers_deliver_expired_and_future_deadlines_once() {
    let reactor = Reactor::new().unwrap();
    let future = now_ns().checked_add(20_000_000).unwrap();
    let mut registrations = Vec::new();
    for (owner, deadline_ns) in [(10, now_ns()), (11, future), (12, now_ns())] {
        // Timer sources do not borrow a native handle.
        registrations.push(
            unsafe {
                reactor.register(
                    WaitSource {
                        kind: KIND_TIMER,
                        interests: 0,
                        handle: 0,
                        deadline_ns,
                    },
                    owner,
                )
            }
            .unwrap(),
        );
    }
    assert!(reactor.cancel(registrations[2]).unwrap());
    assert!(
        reactor
            .notify_completion(registrations[0], COMPLETION, 0)
            .is_err()
    );
    let notifications = ready(&reactor, 2);
    assert!(now_ns() >= future, "timer fired before its deadline");
    let mut owners: Vec<_> = notifications.iter().map(|item| item.owner).collect();
    owners.sort_unstable();
    assert_eq!(owners, [10, 11]);
    for notification in notifications {
        let index = match notification.owner {
            10 => 0,
            11 => 1,
            _ => panic!("unexpected timer owner"),
        };
        same_registration(notification.registration, registrations[index]);
        assert_eq!(notification.events, TIMER);
        assert_eq!(notification.os_error, 0);
        assert!(!reactor.cancel(notification.registration).unwrap());
    }
    assert_eq!(reactor.wait(Some(Duration::ZERO)).unwrap(), 0);
    assert!(reactor.pop_ready().is_none());
}

#[test]
fn cross_thread_completion_wakes_waiter_and_queues_notification() {
    let reactor = Arc::new(Reactor::new().unwrap());
    let registration = completion(&reactor, 42);
    let barrier = Arc::new(Barrier::new(2));
    let waiter = {
        let reactor = Arc::clone(&reactor);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            ready(&reactor, 1)
        })
    };
    barrier.wait();
    // The producer publishes a notification, not a user callback. Consumption
    // remains on the waiter, whether completion wins or loses the wait race.
    assert!(
        reactor
            .notify_completion(registration, COMPLETION | ERROR, 73)
            .unwrap()
    );
    let notifications = waiter.join().unwrap();
    same_registration(notifications[0].registration, registration);
    assert_eq!(notifications[0].owner, 42);
    assert_eq!(notifications[0].events, COMPLETION | ERROR);
    assert_eq!(notifications[0].os_error, 73);
    assert!(
        !reactor
            .notify_completion(registration, COMPLETION, 0)
            .unwrap()
    );
    assert!(reactor.pop_ready().is_none());
}

#[test]
fn cancelled_slots_reject_stale_generations_and_duplicate_completion() {
    let reactor = Reactor::new().unwrap();
    let old = completion(&reactor, 1);
    assert!(reactor.cancel(old).unwrap());
    assert!(!reactor.cancel(old).unwrap());
    let current = completion(&reactor, 2);
    assert_eq!(current.key, old.key);
    assert_ne!(current.generation, old.generation);
    assert!(!reactor.cancel(old).unwrap());
    assert!(!reactor.notify_completion(old, COMPLETION, 0).unwrap());
    assert!(reactor.pop_ready().is_none());
    assert!(reactor.notify_completion(current, COMPLETION, 0).unwrap());
    assert!(!reactor.notify_completion(current, COMPLETION, 0).unwrap());
    assert!(!reactor.cancel(current).unwrap());
    let notification = reactor.pop_ready().unwrap();
    same_registration(notification.registration, current);
    assert_eq!(notification.owner, 2);
    assert!(reactor.pop_ready().is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn unsupported_epoll_source_returns_error_without_retaining_a_registration() {
    use std::os::fd::AsRawFd;
    let file = tempfile::tempfile().unwrap();
    let reactor = Reactor::new().unwrap();
    let source = WaitSource {
        kind: KIND_READINESS,
        interests: READABLE,
        handle: file.as_raw_fd() as u64,
        deadline_ns: 0,
    };
    for _ in 0..2 {
        // SAFETY: The regular file is valid and outlives the reactor, although
        // epoll does not support registering it for readiness.
        let error = unsafe { reactor.register(source, 1) }.unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    }
}

#[cfg(unix)]
fn socket_handle(socket: &TcpStream) -> u64 {
    use std::os::fd::AsRawFd;
    socket.as_raw_fd() as u64
}

#[cfg(windows)]
fn socket_handle(socket: &TcpStream) -> u64 {
    use std::os::windows::io::AsRawSocket;
    #[cfg(target_pointer_width = "32")]
    {
        u64::from(socket.as_raw_socket())
    }
    #[cfg(target_pointer_width = "64")]
    {
        socket.as_raw_socket()
    }
}

#[test]
fn tcp_readiness_merges_disjoint_interests_without_owning_the_socket() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut socket, _) = listener.accept().unwrap();
    socket.set_nonblocking(true).unwrap();
    // Declared after socket: all registrations are dropped before its handle.
    let reactor = Reactor::new().unwrap();
    let source = |interests| WaitSource {
        kind: KIND_READINESS,
        interests,
        handle: socket_handle(&socket),
        deadline_ns: 0,
    };
    // The test retains ownership of socket until the reactor has been dropped.
    let readable = unsafe { reactor.register(source(READABLE), 10) }.unwrap();
    let writable = unsafe { reactor.register(source(WRITABLE), 11) }.unwrap();
    assert!(unsafe { reactor.register(source(READABLE), 12) }.is_err());
    assert!(unsafe { reactor.register(source(READABLE | WRITABLE), 13) }.is_err());
    // Write readiness fires first; the remaining read interest must be rearmed.
    let writable_ready = ready(&reactor, 1)[0];
    same_registration(writable_ready.registration, writable);
    assert_eq!(writable_ready.owner, 11);
    assert_ne!(writable_ready.events & WRITABLE, 0);
    assert_eq!(writable_ready.os_error, 0);
    peer.write_all(b"x").unwrap();
    let readable_ready = ready(&reactor, 1)[0];
    same_registration(readable_ready.registration, readable);
    assert_eq!(readable_ready.owner, 10);
    assert_ne!(readable_ready.events & READABLE, 0);
    assert_eq!(readable_ready.os_error, 0);
    // Both directions are now ready, but a combined registration fires once.
    let combined = unsafe { reactor.register(source(READABLE | WRITABLE), 12) }.unwrap();
    let combined_ready = ready(&reactor, 1)[0];
    same_registration(combined_ready.registration, combined);
    assert_eq!(combined_ready.owner, 12);
    assert_ne!(combined_ready.events & (READABLE | WRITABLE), 0);
    assert_eq!(combined_ready.os_error, 0);
    assert_eq!(reactor.wait(Some(Duration::ZERO)).unwrap(), 0);
    assert!(reactor.pop_ready().is_none());
    let mut byte = [0];
    assert_eq!(socket.read(&mut byte).unwrap(), 1);
    assert_eq!(byte, [b'x']);
    // Cancellation and reactor destruction release registrations, not handles.
    let pending = unsafe {
        reactor.register(
            WaitSource {
                kind: KIND_READINESS,
                interests: READABLE,
                handle: socket_handle(&socket),
                deadline_ns: 0,
            },
            14,
        )
    }
    .unwrap();
    assert!(reactor.cancel(pending).unwrap());
    // Also leave an active borrow for Reactor::drop to unregister.
    unsafe {
        reactor.register(
            WaitSource {
                kind: KIND_READINESS,
                interests: READABLE,
                handle: socket_handle(&socket),
                deadline_ns: 0,
            },
            15,
        )
    }
    .unwrap();
    drop(reactor);
    socket.set_nonblocking(false).unwrap();
    socket.write_all(b"y").unwrap();
    peer.read_exact(&mut byte).unwrap();
    assert_eq!(byte, [b'y']);
}
