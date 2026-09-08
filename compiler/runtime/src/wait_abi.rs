//! Private wait ABI, version 1. All pointers are valid, aligned and non-null;
//! output slots are exclusively writable for the call. A reactor outlives all
//! calls and producers; drop requires their termination. Readiness handles stay
//! open until their registration fires, is cancelled, or the reactor is dropped.
//! No pointer passed here refers to moving Loom storage.
//! One owner calls wait/pop_ready; registration and completion producers may
//! operate concurrently. Output slots are written only on success, except new
//! clears its reactor output to null before attempting creation.
//!
//! Status is 0 on success or a negative OS error (EIO without an OS code).
//! Cancel/notify instead return 1 when applied and 0 for a stale registration.
//! A notify error after publication does not retract the queued completion.

use super::wait::{Reactor, ReadyNotification, Registration, WaitSource, now_ns};
use std::{io, ptr, time::Duration};

fn failure(error: io::Error) -> i32 {
    if error.kind() == io::ErrorKind::OutOfMemory {
        super::fatal("out of memory");
    }
    -error
        .raw_os_error()
        .filter(|code| *code > 0)
        .unwrap_or(libc::EIO)
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_wait_version() -> u32 {
    1
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_wait_now() -> u64 {
    now_ns()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_wait_new(out: *mut *mut Reactor) -> i32 {
    // SAFETY: The caller supplies an exclusive output slot.
    unsafe { out.write(ptr::null_mut()) };
    match Reactor::new() {
        Ok(reactor) => {
            // SAFETY: Ownership transfers to the caller until wait_drop.
            unsafe { out.write(Box::into_raw(Box::new(reactor))) };
            0
        }
        Err(error) => failure(error),
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_wait_drop(reactor: *mut Reactor) {
    // SAFETY: The caller has stopped all operations and releases ownership once.
    unsafe { drop(Box::from_raw(reactor)) };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_wait_register(
    reactor: *const Reactor,
    source: *const WaitSource,
    owner: u64,
    out: *mut Registration,
) -> i32 {
    // SAFETY: Source and reactor are live; the caller retains the native handle.
    let registration = unsafe { (&*reactor).register(*source, owner) };
    match registration {
        Ok(registration) => {
            // SAFETY: The caller supplies an exclusive output slot.
            unsafe { out.write(registration) };
            0
        }
        Err(error) => failure(error),
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_wait_cancel(
    reactor: *const Reactor,
    registration: Registration,
) -> i32 {
    // SAFETY: The caller retains the reactor throughout the call.
    unsafe { (&*reactor).cancel(registration) }.map_or_else(failure, i32::from)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_wait_notify_completion(
    reactor: *const Reactor,
    registration: Registration,
    events: u32,
    os_error: i32,
) -> i32 {
    // SAFETY: A foreign producer retains the reactor, never a managed frame.
    unsafe { (&*reactor).notify_completion(registration, events, os_error) }
        .map_or_else(failure, i32::from)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_wait_wait(reactor: *const Reactor, timeout_ns: i64) -> i32 {
    let timeout = match timeout_ns {
        -1 => None,
        0.. => Some(Duration::from_nanos(timeout_ns as u64)),
        _ => return -libc::EINVAL,
    };
    // SAFETY: The caller retains the reactor throughout the wait.
    unsafe { (&*reactor).wait(timeout) }.map_or_else(failure, |_| 0)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_wait_pop_ready(
    reactor: *const Reactor,
    out: *mut ReadyNotification,
) -> u32 {
    // SAFETY: The caller retains the reactor and supplies an exclusive slot.
    if let Some(notification) = unsafe { (&*reactor).pop_ready() } {
        unsafe { out.write(notification) };
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wait::{COMPLETION, KIND_COMPLETION};
    use std::mem::{MaybeUninit, size_of};

    #[test]
    fn abi_completion_round_trip() {
        assert_eq!(loom_rt_wait_version(), 1);
        assert_eq!(size_of::<WaitSource>(), 24);
        assert_eq!(size_of::<Registration>(), 16);
        assert_eq!(size_of::<ReadyNotification>(), 32);
        let source = WaitSource {
            kind: KIND_COMPLETION,
            interests: 0,
            handle: 0,
            deadline_ns: 0,
        };
        let mut reactor = ptr::null_mut();
        let mut registration = MaybeUninit::uninit();
        let mut ready = MaybeUninit::uninit();
        // SAFETY: Stack output slots are exclusive; reactor ownership is released
        // after its last use. This source does not borrow a native handle.
        unsafe {
            assert_eq!(loom_rt_wait_new(&mut reactor), 0);
            assert_eq!(
                loom_rt_wait_register(reactor, &source, 17, registration.as_mut_ptr()),
                0
            );
            let registration = registration.assume_init();
            assert_eq!(loom_rt_wait_pop_ready(reactor, ready.as_mut_ptr()), 0);
            assert_eq!(
                loom_rt_wait_notify_completion(reactor, registration, COMPLETION, 0),
                1
            );
            assert_eq!(loom_rt_wait_wait(reactor, 0), 0);
            assert_eq!(loom_rt_wait_pop_ready(reactor, ready.as_mut_ptr()), 1);
            assert_eq!(ready.assume_init().owner, 17);
            assert_eq!(loom_rt_wait_cancel(reactor, registration), 0);
            assert_eq!(loom_rt_wait_wait(reactor, -2), -libc::EINVAL);
            loom_rt_wait_drop(reactor);
        }
    }
}
