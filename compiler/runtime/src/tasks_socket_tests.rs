use super::outcomes::{loom_rt_task_cancel_begin, loom_rt_task_status};
use super::socket::{
    Socket, loom_rt_socket_accept, loom_rt_socket_close, loom_rt_socket_listen,
    loom_rt_task_wait_socket,
};
use super::*;
use crate::{loom_rt_box_new, loom_rt_text_new, rooted};
use std::io::Write;
use std::mem::size_of;
use std::net::TcpStream;
use std::thread::JoinHandle;

thread_local! {
    static PEER: RefCell<Option<JoinHandle<()>>> = const { RefCell::new(None) };
}

#[repr(C)]
struct Frame {
    state: u64,
    listener: i64,
    child: u64,
}

fn with_owner<R>(run: impl FnOnce(&Owner) -> R) -> R {
    assert!(!OWNER.get().is_null());
    run(unsafe { &*OWNER.get() })
}

unsafe fn task(resume: Resume) -> u64 {
    let frame = loom_rt_box_new(size_of::<Frame>(), None);
    unsafe { loom_rt_task_create(frame, resume, ptr::null(), 0) }
}

unsafe fn listen() -> i64 {
    let address = unsafe { loom_rt_text_new(b"127.0.0.1:0".as_ptr(), 11) };
    let token = unsafe { loom_rt_socket_listen(address) };
    assert!(token > 0);
    token
}

unsafe extern "C-unwind" fn ready(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            let token = listen();
            // Creating the address Text may have moved the rooted frame.
            (*(*slots).cast::<Frame>()).listener = token;
            assert_eq!(loom_rt_socket_accept(token), -2);
            let address = with_owner(|owner| {
                let socket = owner.sockets().get(token).unwrap();
                let Socket::Listener(listener) = socket.as_ref() else {
                    panic!("expected listener token");
                };
                listener.local_addr().unwrap()
            });
            PEER.with(|peer| {
                *peer.borrow_mut() = Some(std::thread::spawn(move || {
                    let mut connection = TcpStream::connect(address).unwrap();
                    connection.write_all(b"loopback").unwrap();
                }));
            });
            (*(*slots).cast::<Frame>()).state = 1;
            assert_eq!(loom_rt_task_wait_socket(token, 1), 0);
            assert_eq!(loom_rt_socket_close(token), -1);
            return 1;
        }
        let token = (*(*slots).cast::<Frame>()).listener;
        // The OS registration has fired, but its lease remains until the task
        // consumes the notification. Closing now must still be rejected.
        assert_eq!(loom_rt_socket_close(token), -1);
        assert_eq!(loom_rt_task_wait_socket(token, 1), 1);
        let accepted = loom_rt_socket_accept(token);
        assert!(accepted > token);
        with_owner(|owner| {
            let socket = owner.sockets().get(accepted).unwrap();
            assert!(matches!(socket.as_ref(), Socket::Stream(_)));
        });
        assert_eq!(loom_rt_socket_close(accepted), 0);
        assert_eq!(loom_rt_socket_close(token), 0);
        assert_eq!(loom_rt_socket_close(token), -1);
        let replacement = listen();
        assert!(replacement > accepted);
        assert_eq!(loom_rt_socket_accept(token), -1);
        assert_eq!(loom_rt_socket_close(replacement), 0);
        0
    })
}

unsafe extern "C-unwind" fn construct_ready() -> u64 {
    unsafe { task(ready) }
}

#[test]
fn loopback_readiness_keeps_exact_socket_alive_until_consumed() {
    unsafe { loom_rt_task_run(construct_ready) };
    PEER.with(|peer| peer.borrow_mut().take().unwrap().join().unwrap());
}

unsafe extern "C-unwind" fn waiting_child(frame: *mut u8) -> i64 {
    let token = unsafe { (*frame.cast::<Frame>()).listener };
    assert_eq!(loom_rt_task_wait_socket(token, 1), 0);
    1
}

unsafe extern "C-unwind" fn cancelled(frame: *mut u8) -> i64 {
    rooted([frame], |slots| unsafe {
        if (*(*slots).cast::<Frame>()).state == 0 {
            let token = listen();
            (*(*slots).cast::<Frame>()).listener = token;
            let child_frame = loom_rt_box_new(size_of::<Frame>(), None);
            (*child_frame.cast::<Frame>()).listener = token;
            let child = loom_rt_task_create(child_frame, waiting_child, ptr::null(), 0);
            (*(*slots).cast::<Frame>()).child = child;
            (*(*slots).cast::<Frame>()).state = 1;
        }
        if loom_rt_task_wait_timer(0) == 0 {
            return 1;
        }
        let token = (*(*slots).cast::<Frame>()).listener;
        let child = (*(*slots).cast::<Frame>()).child;
        with_owner(|owner| {
            let core = owner.core.borrow();
            assert!(matches!(core.tasks[&child].state, State::ExternalWaiting));
            assert_eq!(core.pending, 1);
        });
        assert_eq!(loom_rt_socket_close(token), -1);
        loom_rt_task_cancel_begin(child);
        assert_eq!(loom_rt_task_status(child), 2);
        loom_rt_task_release(child);
        assert_eq!(loom_rt_socket_close(token), 0);
        0
    })
}

unsafe extern "C-unwind" fn construct_cancelled() -> u64 {
    unsafe { task(cancelled) }
}

#[test]
fn cancelling_a_waiting_child_releases_lease_before_close() {
    unsafe { loom_rt_task_run(construct_cancelled) };
}
