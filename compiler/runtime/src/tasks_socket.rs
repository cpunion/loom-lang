//! Owner-local socket tokens. The reactor only borrows native handles; an
//! external task wait keeps an Arc lease until readiness or cancellation has
//! retired its registration. Tokens are never raw descriptors or reused.

use super::*;
use crate::wait::{KIND_READINESS, READABLE, WRITABLE};
use std::net::{SocketAddr, TcpListener, TcpStream};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

pub(super) enum Socket {
    Listener(TcpListener),
    Stream(TcpStream),
}

impl Socket {
    fn handle(&self) -> u64 {
        #[cfg(unix)]
        {
            match self {
                Self::Listener(socket) => socket.as_raw_fd() as u64,
                Self::Stream(socket) => socket.as_raw_fd() as u64,
            }
        }
        #[cfg(windows)]
        {
            match self {
                Self::Listener(socket) => socket.as_raw_socket() as u64,
                Self::Stream(socket) => socket.as_raw_socket() as u64,
            }
        }
    }

    fn source(&self, interests: i64) -> Option<WaitSource> {
        let interests = u32::try_from(interests).ok()?;
        if interests != READABLE && interests != WRITABLE {
            return None;
        }
        if matches!(self, Self::Listener(_)) && interests != READABLE {
            return None;
        }
        Some(WaitSource {
            kind: KIND_READINESS,
            interests,
            handle: self.handle(),
            deadline_ns: 0,
        })
    }
}

#[derive(Default)]
pub(super) struct Sockets {
    handles: RefCell<HashMap<i64, Arc<Socket>>>,
    next: Cell<i64>,
}

impl Sockets {
    fn insert(&self, socket: Socket) -> i64 {
        let token = self
            .next
            .get()
            .checked_add(1)
            .unwrap_or_else(|| fatal("socket identities exhausted"));
        self.handles.borrow_mut().insert(token, Arc::new(socket));
        self.next.set(token);
        token
    }

    pub(super) fn get(&self, token: i64) -> Option<Arc<Socket>> {
        self.handles.borrow().get(&token).cloned()
    }

    fn listen(&self, address: &str) -> i64 {
        // Keep the owner thread out of name resolution; a future source API
        // can put DNS/connect work on the existing bounded worker path.
        let Ok(address) = address.parse::<SocketAddr>() else {
            return -1;
        };
        let Ok(listener) = TcpListener::bind(address) else {
            return -1;
        };
        if listener.set_nonblocking(true).is_err() {
            return -1;
        }
        self.insert(Socket::Listener(listener))
    }

    fn accept(&self, token: i64) -> i64 {
        let Some(socket) = self.get(token) else {
            return -1;
        };
        let Socket::Listener(listener) = socket.as_ref() else {
            return -1;
        };
        match listener.accept() {
            Ok((stream, _)) => {
                if stream.set_nonblocking(true).is_err() {
                    return -1;
                }
                self.insert(Socket::Stream(stream))
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => -2,
            Err(_) => -1,
        }
    }

    fn close(&self, token: i64) -> bool {
        let mut handles = self.handles.borrow_mut();
        let Some(socket) = handles.get(&token) else {
            return false;
        };
        // Reject a close while an active or delivered wait still leases the
        // exact handle. Cancellation/ready consumption drops that lease first.
        if Arc::strong_count(socket) != 1 {
            return false;
        }
        handles.remove(&token);
        true
    }
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_listen(address: *const u8) -> i64 {
    // SAFETY: Generated code supplies a rooted, valid UTF-8 Text for this call.
    let address = unsafe { std::str::from_utf8_unchecked(crate::text_bytes(address)) };
    edit(|owner, _| Ok(owner.sockets().listen(address)))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_accept(token: i64) -> i64 {
    // -2 means a nonblocking accept would block; other failures return -1.
    edit(|owner, _| Ok(owner.sockets().accept(token)))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_close(token: i64) -> i64 {
    edit(|owner, _| Ok(if owner.sockets().close(token) { 0 } else { -1 }))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_wait_socket(token: i64, interests: i64) -> i32 {
    let lease = edit(|owner, _| Ok(owner.sockets().get(token)))
        .unwrap_or_else(|| fault("socket wait requires a live token"));
    let source = lease
        .source(interests)
        .unwrap_or_else(|| fault("invalid socket readiness interest"));
    // SAFETY: The external wait owns `lease` until the reactor retires or
    // cancels the registration; no moving Loom pointer enters the poller.
    i32::from(unsafe { wait_source(source, Some(lease)) }.is_some())
}
