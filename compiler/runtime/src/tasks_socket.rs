//! Owner-local socket tokens. The reactor only borrows native handles; an
//! external task wait keeps an owner-local Rc lease until readiness or cancellation has
//! retired its registration. Tokens are never raw descriptors or reused.

use super::*;
use crate::wait::{KIND_READINESS, READABLE, WRITABLE};
use crate::{buffer_bytes, reserve};
use socket2::{Domain, Protocol, SockAddr, Socket as NativeSocket, Type};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

#[cfg(windows)]
fn raw_handle(socket: &impl AsRawSocket) -> u64 {
    #[cfg(target_pointer_width = "64")]
    {
        socket.as_raw_socket()
    }
    #[cfg(target_pointer_width = "32")]
    {
        socket.as_raw_socket() as u64
    }
}

pub(super) enum Socket {
    Listener(TcpListener),
    Stream {
        stream: TcpStream,
        connecting: Cell<bool>,
    },
}

impl Socket {
    fn handle(&self) -> u64 {
        #[cfg(unix)]
        {
            match self {
                Self::Listener(socket) => socket.as_raw_fd() as u64,
                Self::Stream { stream, .. } => stream.as_raw_fd() as u64,
            }
        }
        #[cfg(windows)]
        {
            match self {
                Self::Listener(socket) => raw_handle(socket),
                Self::Stream { stream, .. } => raw_handle(stream),
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
        if matches!(self, Self::Stream { connecting, .. } if connecting.get())
            && interests != WRITABLE
        {
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
    handles: RefCell<HashMap<i64, Rc<Socket>>>,
    next: Cell<i64>,
}

impl Sockets {
    fn insert(&self, socket: Socket) -> i64 {
        let token = self
            .next
            .get()
            .checked_add(1)
            .unwrap_or_else(|| fatal("socket identities exhausted"));
        self.handles.borrow_mut().insert(token, Rc::new(socket));
        self.next.set(token);
        token
    }

    pub(super) fn get(&self, token: i64) -> Option<Rc<Socket>> {
        self.handles.borrow().get(&token).cloned()
    }

    fn listen(&self, address: &str) -> i64 {
        // Keep the owner thread out of name resolution.
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
                self.insert(Socket::Stream {
                    stream,
                    connecting: Cell::new(false),
                })
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => -2,
            Err(_) => -1,
        }
    }

    fn connect(&self, address: &str) -> i64 {
        // Numeric addresses only: creating and initiating a nonblocking socket
        // cannot resolve a name or wait for a remote handshake on this owner.
        let Ok(address) = address.parse::<SocketAddr>() else {
            return -1;
        };
        let Ok(socket) = NativeSocket::new(
            Domain::for_address(address),
            Type::STREAM,
            Some(Protocol::TCP),
        ) else {
            return -1;
        };
        if socket.set_nonblocking(true).is_err() {
            return -1;
        }
        let connecting = match socket.connect(&SockAddr::from(address)) {
            Ok(()) => false,
            // Rust maps WSAEWOULDBLOCK to WouldBlock on Windows.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => true,
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::EINPROGRESS) => true,
            Err(_) => return -1,
        };
        self.insert(Socket::Stream {
            stream: socket.into(),
            connecting: Cell::new(connecting),
        })
    }

    fn connect_status(&self, token: i64) -> i64 {
        let Some(socket) = self.get(token) else {
            return -1;
        };
        let Socket::Stream { stream, connecting } = socket.as_ref() else {
            return -1;
        };
        if !connecting.get() {
            return 0;
        }
        match stream.take_error() {
            Ok(Some(_)) | Err(_) => -1,
            Ok(None) => match stream.peer_addr() {
                Ok(_) => {
                    connecting.set(false);
                    0
                }
                Err(_) => -2,
            },
        }
    }

    fn close(&self, token: i64) -> bool {
        let mut handles = self.handles.borrow_mut();
        let Some(socket) = handles.get(&token) else {
            return false;
        };
        // Reject a close while an active or delivered wait still leases the
        // exact handle. Cancellation/ready consumption drops that lease first.
        if Rc::strong_count(socket) != 1 {
            return false;
        }
        handles.remove(&token);
        true
    }

    fn local_port(&self, token: i64) -> i64 {
        let Some(socket) = self.get(token) else {
            return -1;
        };
        let Socket::Listener(listener) = socket.as_ref() else {
            return -1;
        };
        listener
            .local_addr()
            .map_or(-1, |address| i64::from(address.port()))
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
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_connect(address: *const u8) -> i64 {
    // SAFETY: Generated code supplies a rooted, valid UTF-8 Text for this call.
    let address = unsafe { std::str::from_utf8_unchecked(crate::text_bytes(address)) };
    edit(|owner, _| Ok(owner.sockets().connect(address)))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_connect_status(token: i64) -> i64 {
    edit(|owner, _| Ok(owner.sockets().connect_status(token)))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_close(token: i64) -> i64 {
    edit(|owner, _| Ok(if owner.sockets().close(token) { 0 } else { -1 }))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_local_port(token: i64) -> i64 {
    edit(|owner, _| Ok(owner.sockets().local_port(token)))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_read(
    token: i64,
    bytes: *mut u8,
    limit: i64,
) -> i64 {
    let Ok(limit) = usize::try_from(limit) else {
        return -1;
    };
    if limit == 0 {
        return 0;
    }
    let Some(socket) = edit(|owner, _| Ok(owner.sockets().get(token))) else {
        return -1;
    };
    let Socket::Stream { stream, connecting } = socket.as_ref() else {
        return -1;
    };
    if connecting.get() {
        return -1;
    }
    // A bounded native scratch buffer avoids keeping a movable Bytes pointer
    // across I/O. Only a successful read grows the caller's rooted Bytes.
    let mut scratch = vec![0; limit.min(64 * 1024)];
    let mut stream = stream;
    match stream.read(&mut scratch) {
        Ok(0) => 0,
        Ok(count) => {
            // SAFETY: Generated code roots bytes. reserve may collect and
            // returns the relocated header before we copy initialized data.
            unsafe {
                let buffer = reserve(bytes, count, 1);
                ptr::copy_nonoverlapping(
                    scratch.as_ptr(),
                    (*buffer).data.add((*buffer).len),
                    count,
                );
                (*buffer).len += count;
            }
            count as i64
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => -2,
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_write_bytes(
    token: i64,
    bytes: *const u8,
    offset: i64,
) -> i64 {
    let Ok(offset) = usize::try_from(offset) else {
        return -1;
    };
    // SAFETY: This call does not retain or relocate the rooted Bytes value.
    let bytes = unsafe { buffer_bytes(bytes) };
    let Some(bytes) = bytes.get(offset..) else {
        return -1;
    };
    let Some(socket) = edit(|owner, _| Ok(owner.sockets().get(token))) else {
        return -1;
    };
    let Socket::Stream { stream, connecting } = socket.as_ref() else {
        return -1;
    };
    if connecting.get() {
        return -1;
    }
    let mut stream = stream;
    match stream.write(bytes) {
        Ok(count) => count as i64,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => -2,
        Err(_) => -1,
    }
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
