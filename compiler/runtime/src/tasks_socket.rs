//! Owner-local socket tokens. The reactor only borrows native handles; an
//! external task wait keeps an owner-local Rc lease until readiness or cancellation has
//! retired its registration. Tokens are never raw descriptors or reused.

use super::*;
use crate::wait::{ERROR, KIND_READINESS, READABLE, WRITABLE};
use crate::{buffer_bytes, reserve};
use socket2::{
    Domain, MaybeUninitSlice, Protocol, SockAddr, SockRef, Socket as NativeSocket, TcpKeepalive,
    Type,
};
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Connection {
    Pending,
    Notified,
    Connected,
    Failed,
}

pub(super) enum Socket {
    Listener(TcpListener),
    Datagram(UdpSocket),
    Stream {
        stream: TcpStream,
        connection: Cell<Connection>,
    },
}

impl Socket {
    pub(super) fn ready(&self, events: u32) {
        if let Self::Stream { connection, .. } = self {
            // IOCP can report CONNECT_FAIL independently of a later SO_ERROR
            // read. Keep this terminal outcome instead of rearming forever.
            if connection.get() == Connection::Pending {
                if events & ERROR != 0 {
                    connection.set(Connection::Failed);
                } else if events & WRITABLE != 0 {
                    connection.set(Connection::Notified);
                }
            }
        }
    }

    fn handle(&self) -> u64 {
        #[cfg(unix)]
        {
            match self {
                Self::Listener(socket) => socket.as_raw_fd() as u64,
                Self::Datagram(socket) => socket.as_raw_fd() as u64,
                Self::Stream { stream, .. } => stream.as_raw_fd() as u64,
            }
        }
        #[cfg(windows)]
        {
            match self {
                Self::Listener(socket) => raw_handle(socket),
                Self::Datagram(socket) => raw_handle(socket),
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
        if matches!(self, Self::Stream { connection, .. } if connection.get() != Connection::Connected)
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
                    connection: Cell::new(Connection::Connected),
                })
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => -2,
            Err(_) => -1,
        }
    }

    fn bind_datagram(&self, address: &str) -> i64 {
        let Ok(address) = address.parse::<SocketAddr>() else {
            return -1;
        };
        let Ok(socket) = UdpSocket::bind(address) else {
            return -1;
        };
        if socket.set_nonblocking(true).is_err() {
            return -1;
        }
        self.insert(Socket::Datagram(socket))
    }

    fn connect_datagram(&self, address: &str) -> i64 {
        let Ok(address) = address.parse::<SocketAddr>() else {
            return -1;
        };
        if address.port() == 0 {
            return -1;
        }
        let binding = if address.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let Ok(socket) = UdpSocket::bind(binding) else {
            return -1;
        };
        if socket.set_nonblocking(true).is_err() || socket.connect(address).is_err() {
            return -1;
        }
        self.insert(Socket::Datagram(socket))
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
        let connection = match socket.connect(&SockAddr::from(address)) {
            Ok(()) => Connection::Connected,
            // Rust maps WSAEWOULDBLOCK to WouldBlock on Windows.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Connection::Pending,
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::EINPROGRESS) => Connection::Pending,
            Err(_) => return -1,
        };
        self.insert(Socket::Stream {
            stream: socket.into(),
            connection: Cell::new(connection),
        })
    }

    fn connect_status(&self, token: i64) -> i64 {
        let Some(socket) = self.get(token) else {
            return -1;
        };
        let Socket::Stream { stream, connection } = socket.as_ref() else {
            return -1;
        };
        match connection.get() {
            Connection::Connected => return 0,
            Connection::Failed => return -1,
            Connection::Pending => return -2,
            Connection::Notified => {}
        }
        match stream.take_error() {
            Ok(Some(_)) | Err(_) => {
                connection.set(Connection::Failed);
                -1
            }
            Ok(None) => match stream.peer_addr() {
                Ok(_) => {
                    connection.set(Connection::Connected);
                    0
                }
                Err(_) => {
                    connection.set(Connection::Failed);
                    -1
                }
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

    fn retire_io(&self, token: i64) -> Option<Rc<Socket>> {
        let mut handles = self.handles.borrow_mut();
        if !matches!(
            handles.get(&token)?.as_ref(),
            Socket::Stream { .. } | Socket::Datagram(_)
        ) {
            return None;
        }
        handles.remove(&token)
    }

    fn local_port(&self, token: i64) -> i64 {
        self.address(token, false)
            .map_or(-1, |address| i64::from(address.port()))
    }

    fn datagram_broadcast(&self, token: i64, mode: i64) -> i64 {
        let Some(socket) = self.get(token) else {
            return -1;
        };
        let Socket::Datagram(socket) = socket.as_ref() else {
            return -1;
        };
        match mode {
            -1 => socket.broadcast().map_or(-1, i64::from),
            0 | 1 => socket.set_broadcast(mode == 1).map_or(-1, |()| 0),
            _ => -1,
        }
    }

    fn membership_v4(&self, token: i64, group: &str, interface: &str, join: i64) -> i64 {
        let (Ok(group), Ok(interface)) = (group.parse::<Ipv4Addr>(), interface.parse::<Ipv4Addr>())
        else {
            return -1;
        };
        let Some(socket) = self.get(token) else {
            return -1;
        };
        let Socket::Datagram(socket) = socket.as_ref() else {
            return -1;
        };
        if !group.is_multicast() || !socket.local_addr().is_ok_and(|address| address.is_ipv4()) {
            return -1;
        }
        match join {
            0 => socket.leave_multicast_v4(&group, &interface),
            1 => socket.join_multicast_v4(&group, &interface),
            _ => return -1,
        }
        .map_or(-1, |()| 0)
    }

    fn membership_v6(&self, token: i64, group: &str, interface: i64, join: i64) -> i64 {
        let (Ok(group), Ok(interface)) = (group.parse::<Ipv6Addr>(), u32::try_from(interface))
        else {
            return -1;
        };
        let Some(socket) = self.get(token) else {
            return -1;
        };
        let Socket::Datagram(socket) = socket.as_ref() else {
            return -1;
        };
        if !group.is_multicast() || !socket.local_addr().is_ok_and(|address| address.is_ipv6()) {
            return -1;
        }
        match join {
            0 => socket.leave_multicast_v6(&group, interface),
            1 => socket.join_multicast_v6(&group, interface),
            _ => return -1,
        }
        .map_or(-1, |()| 0)
    }

    fn address(&self, token: i64, peer: bool) -> Option<SocketAddr> {
        let socket = self.get(token)?;
        match socket.as_ref() {
            Socket::Listener(listener) if !peer => listener.local_addr().ok(),
            Socket::Datagram(socket) => {
                if peer {
                    socket.peer_addr().ok()
                } else {
                    socket.local_addr().ok()
                }
            }
            Socket::Stream { stream, connection } if connection.get() == Connection::Connected => {
                if peer {
                    stream.peer_addr().ok()
                } else {
                    stream.local_addr().ok()
                }
            }
            _ => None,
        }
    }

    fn configure_stream(
        &self,
        token: i64,
        apply: impl FnOnce(&TcpStream) -> io::Result<()>,
    ) -> i64 {
        let Some(socket) = self.get(token) else {
            return -1;
        };
        let Socket::Stream { stream, connection } = socket.as_ref() else {
            return -1;
        };
        if connection.get() != Connection::Connected {
            return -1;
        }
        // These operations do not revoke or replace the native handle. In
        // particular a write shutdown must preserve an outstanding read lease.
        apply(stream).map_or(-1, |()| 0)
    }

    fn keepalive(&self, token: i64, enabled: i64, idle: i64, interval: i64, retries: i64) -> i64 {
        if enabled == 0 {
            return self
                .configure_stream(token, |stream| SockRef::from(stream).set_keepalive(false));
        }
        // socket2 narrows or clamps these OS values. Reject unrepresentable
        // requests before changing any option instead of silently shortening it.
        let (Ok(idle), Ok(interval), Ok(retries)) = (
            i32::try_from(idle),
            i32::try_from(interval),
            i32::try_from(retries),
        ) else {
            return -1;
        };
        if enabled != 1 || idle <= 0 || interval <= 0 || retries <= 0 {
            return -1;
        }
        #[cfg(windows)]
        if idle as u64 * 1000 >= u64::from(u32::MAX)
            || interval as u64 * 1000 >= u64::from(u32::MAX)
        {
            return -1;
        }
        let options = TcpKeepalive::new()
            .with_time(Duration::from_secs(idle as u64))
            .with_interval(Duration::from_secs(interval as u64))
            .with_retries(retries as u32);
        self.configure_stream(token, |stream| {
            SockRef::from(stream).set_tcp_keepalive(&options)
        })
    }
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_listen(address: *const u8) -> i64 {
    // SAFETY: Generated code supplies a rooted, valid UTF-8 Text for this call.
    let address = unsafe { std::str::from_utf8_unchecked(crate::text_bytes(address)) };
    edit(|owner, _| Ok(owner.sockets().listen(address)))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_bind_datagram(address: *const u8) -> i64 {
    // SAFETY: Generated code supplies rooted UTF-8 Text; no GC allocation here.
    let address = unsafe { std::str::from_utf8_unchecked(crate::text_bytes(address)) };
    edit(|owner, _| Ok(owner.sockets().bind_datagram(address)))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_connect_datagram(address: *const u8) -> i64 {
    // SAFETY: Generated code supplies rooted UTF-8 Text; no managed allocation.
    let address = unsafe { std::str::from_utf8_unchecked(crate::text_bytes(address)) };
    edit(|owner, _| Ok(owner.sockets().connect_datagram(address)))
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
pub(super) extern "C-unwind" fn loom_rt_socket_abort(token: i64) -> i64 {
    edit(|owner, _| {
        let Some(socket) = owner.sockets().retire_io(token) else {
            return Ok(-1);
        };
        // Revoke the token first, remove native interest while its Rc leases
        // remain alive, and wake waiters through ordinary ready notifications.
        if let Some(reactor) = owner.reactor.get() {
            reactor
                .revoke_readiness(socket.handle())
                .unwrap_or_else(|_| fatal("failed to retire socket wait"));
        }
        Ok(0)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_local_port(token: i64) -> i64 {
    edit(|owner, _| Ok(owner.sockets().local_port(token)))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_datagram_broadcast(token: i64, mode: i64) -> i64 {
    edit(|owner, _| Ok(owner.sockets().datagram_broadcast(token, mode)))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_membership_v4(
    token: i64,
    group: *const u8,
    interface: *const u8,
    join: i64,
) -> i64 {
    // SAFETY: Generated code roots both UTF-8 Text values. This synchronous
    // socket option neither collects managed data nor retains their pointers.
    let group = unsafe { std::str::from_utf8_unchecked(crate::text_bytes(group)) };
    let interface = unsafe { std::str::from_utf8_unchecked(crate::text_bytes(interface)) };
    edit(|owner, _| Ok(owner.sockets().membership_v4(token, group, interface, join)))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_membership_v6(
    token: i64,
    group: *const u8,
    interface: i64,
    join: i64,
) -> i64 {
    // SAFETY: The rooted Text is borrowed only during this non-collecting call.
    let group = unsafe { std::str::from_utf8_unchecked(crate::text_bytes(group)) };
    edit(|owner, _| Ok(owner.sockets().membership_v6(token, group, interface, join)))
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_address(token: i64, peer: i64) -> *mut u8 {
    let address = edit(|owner, _| {
        Ok(match peer {
            0 | 1 => owner.sockets().address(token, peer == 1),
            _ => None,
        })
    });
    let text = address.map_or_else(String::new, |address| address.to_string());
    // SAFETY: Native String bytes remain independent of moving GC throughout
    // allocation. No owner/table borrow or managed interior pointer survives.
    unsafe { crate::loom_rt_text_new(text.as_ptr(), text.len()) }
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_set_nodelay(token: i64, enabled: i64) -> i64 {
    if enabled != 0 && enabled != 1 {
        return -1;
    }
    edit(|owner, _| {
        Ok(owner
            .sockets()
            .configure_stream(token, |stream| stream.set_nodelay(enabled == 1)))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_set_keepalive(
    token: i64,
    enabled: i64,
    idle: i64,
    interval: i64,
    retries: i64,
) -> i64 {
    edit(|owner, _| {
        Ok(owner
            .sockets()
            .keepalive(token, enabled, idle, interval, retries))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_socket_shutdown_write(token: i64) -> i64 {
    edit(|owner, _| {
        Ok(owner
            .sockets()
            .configure_stream(token, |stream| stream.shutdown(Shutdown::Write)))
    })
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
    let Socket::Stream { stream, connection } = socket.as_ref() else {
        return -1;
    };
    if connection.get() != Connection::Connected {
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
    end: i64,
) -> i64 {
    let (Ok(offset), Ok(end)) = (usize::try_from(offset), usize::try_from(end)) else {
        return -1;
    };
    // SAFETY: This call does not retain or relocate the rooted Bytes value.
    let bytes = unsafe { buffer_bytes(bytes) };
    // A suspended source writer retains its initial end even if an alias
    // appends to the shared buffer before a readiness retry.
    let Some(bytes) = bytes.get(offset..end) else {
        return -1;
    };
    let Some(socket) = edit(|owner, _| Ok(owner.sockets().get(token))) else {
        return -1;
    };
    let Socket::Stream { stream, connection } = socket.as_ref() else {
        return -1;
    };
    if connection.get() != Connection::Connected {
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
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_receive_from(
    token: i64,
    bytes: *mut u8,
    limit: i64,
    sender: *mut u8,
) -> i64 {
    let Ok(limit) = usize::try_from(limit) else {
        return -1;
    };
    if limit > 65_535 || bytes == sender {
        return -1;
    }
    let Some(socket) = edit(|owner, _| Ok(owner.sockets().get(token))) else {
        return -1;
    };
    let Socket::Datagram(socket) = socket.as_ref() else {
        return -1;
    };
    // No movable managed pointer crosses the OS read. socket2 reports Unix
    // MSG_TRUNC and Windows WSAEMSGSIZE through the same received flags.
    let mut scratch = vec![MaybeUninit::<u8>::uninit(); limit + 1];
    let mut buffers = [MaybeUninitSlice::new(&mut scratch)];
    let (count, flags, address) = match SockRef::from(socket).recv_from_vectored(&mut buffers) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return -2,
        Err(_) => return -1,
    };
    if flags.is_truncated() || count > limit {
        // The datagram is consumed, but neither output receives a partial packet.
        return -3;
    }
    let Some(address) = address.as_socket() else {
        return -1;
    };
    let address = address.to_string();
    crate::rooted([bytes, sender], |slots| unsafe {
        // Reload each argument after allocations; reserving the second output
        // can move the first header. Both input handles stay rooted throughout.
        let buffer = reserve(*slots, count, 1);
        ptr::copy_nonoverlapping(
            scratch.as_ptr().cast::<u8>(),
            (*buffer).data.add((*buffer).len),
            count,
        );
        (*buffer).len += count;
        let peer = reserve(*slots.add(1), address.len(), 1);
        ptr::copy_nonoverlapping(
            address.as_ptr(),
            (*peer).data.add((*peer).len),
            address.len(),
        );
        (*peer).len += address.len();
    });
    count as i64
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_send_to(
    token: i64,
    bytes: *const u8,
    end: i64,
    address: *const u8,
) -> i64 {
    let Ok(end) = usize::try_from(end) else {
        return -1;
    };
    // SAFETY: Generated code roots both inputs. This nonblocking operation
    // neither allocates managed data nor retains a managed interior pointer.
    let bytes = unsafe { buffer_bytes(bytes) };
    let Some(bytes) = bytes.get(..end) else {
        return -1;
    };
    let address = unsafe { std::str::from_utf8_unchecked(crate::text_bytes(address)) };
    let Ok(address) = address.parse::<SocketAddr>() else {
        return -1;
    };
    let Some(socket) = edit(|owner, _| Ok(owner.sockets().get(token))) else {
        return -1;
    };
    let Socket::Datagram(socket) = socket.as_ref() else {
        return -1;
    };
    match socket.send_to(bytes, address) {
        Ok(count) => count as i64,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => -2,
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C-unwind" fn loom_rt_socket_send_datagram(
    token: i64,
    bytes: *const u8,
    end: i64,
) -> i64 {
    let Ok(end) = usize::try_from(end) else {
        return -1;
    };
    // SAFETY: Generated code roots Bytes. This nonblocking send neither
    // allocates managed data nor retains an interior pointer.
    let Some(bytes) = (unsafe { buffer_bytes(bytes) }).get(..end) else {
        return -1;
    };
    let Some(socket) = edit(|owner, _| Ok(owner.sockets().get(token))) else {
        return -1;
    };
    let Socket::Datagram(socket) = socket.as_ref() else {
        return -1;
    };
    match socket.send(bytes) {
        Ok(count) => count as i64,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => -2,
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub(super) extern "C-unwind" fn loom_rt_task_wait_socket(token: i64, interests: i64) -> i32 {
    let lease = edit(|owner, core| {
        let id = core.current.ok_or("socket wait outside a resume")?;
        if let Some(lease) = owner.sockets().get(token) {
            return Ok(Some(lease));
        }
        if !matches!(
            core.tasks[&id].state,
            State::Running | State::ExternalWaiting
        ) {
            return Err("task already awaits a child");
        }
        if let Some(wait) = core.tasks[&id].external.as_ref() {
            if wait.source.kind != KIND_READINESS || wait.socket.is_none() {
                return Err("task already has another external wait");
            }
        }
        // An abort can happen before the wait Task starts or after its wait
        // was delivered. Consume that old registration without rearming it.
        owner.cancel_wait(core, id);
        core.tasks.get_mut(&id).unwrap().state = State::Running;
        Ok(None)
    });
    let Some(lease) = lease else { return 1 };
    let source = lease
        .source(interests)
        .unwrap_or_else(|| fault("invalid socket readiness interest"));
    // SAFETY: The external wait owns `lease` until the reactor retires or
    // cancels the registration; no moving Loom pointer enters the poller.
    i32::from(unsafe { wait_source(source, Some(lease)) }.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multicast_membership_preserves_identity_and_checks_native_inputs() {
        let sockets = Sockets::default();
        let token = sockets.bind_datagram("0.0.0.0:0");
        assert!(token > 0);
        let lease = sockets.get(token).unwrap();
        assert_eq!(sockets.membership_v4(token, "239.255.0.1", "0.0.0.0", 1), 0);
        assert!(!sockets.close(token));
        assert_eq!(sockets.membership_v4(token, "239.255.0.1", "0.0.0.0", 0), 0);
        for (group, interface, join) in [
            ("127.0.0.1", "0.0.0.0", 1),
            ("239.255.0.1:1234", "0.0.0.0", 1),
            ("239.255.0.1", "localhost", 1),
            ("239.255.0.1", "0.0.0.0", 2),
        ] {
            assert_eq!(sockets.membership_v4(token, group, interface, join), -1);
        }
        assert_eq!(sockets.membership_v6(token, "ff02::1", 0, 1), -1);
        drop(lease);
        assert!(sockets.close(token));
        assert_eq!(
            sockets.membership_v4(token, "239.255.0.1", "0.0.0.0", 1),
            -1
        );
        let listener = sockets.listen("127.0.0.1:0");
        assert_eq!(
            sockets.membership_v4(listener, "239.255.0.1", "0.0.0.0", 1),
            -1
        );
        assert!(sockets.close(listener));
        let token = sockets.bind_datagram("[::]:0");
        if token > 0 {
            for (group, interface, join) in [
                ("::1", 0, 1),
                ("ff02::1", -1, 1),
                ("ff02::1", i64::from(u32::MAX) + 1, 1),
                ("ff02::1%1", 0, 1),
                ("ff02::1", 0, 2),
            ] {
                assert_eq!(sockets.membership_v6(token, group, interface, join), -1);
            }
            assert_eq!(
                sockets.membership_v4(token, "239.255.0.1", "0.0.0.0", 1),
                -1
            );
            assert!(sockets.close(token));
        }
    }

    #[test]
    fn broadcast_options_share_identity_without_retiring_leases() {
        let sockets = Sockets::default();
        let token = sockets.bind_datagram("127.0.0.1:0");
        assert!(token > 0);
        let lease = sockets.get(token).unwrap();
        assert_eq!(sockets.datagram_broadcast(token, -1), 0);
        for enabled in [1, 0] {
            assert_eq!(sockets.datagram_broadcast(token, enabled), 0);
            assert_eq!(sockets.datagram_broadcast(token, -1), enabled);
            assert!(!sockets.close(token));
        }
        assert_eq!(sockets.datagram_broadcast(token, 2), -1);
        drop(lease);
        assert!(sockets.close(token));
        assert_eq!(sockets.datagram_broadcast(token, -1), -1);
        let listener = sockets.listen("127.0.0.1:0");
        assert_eq!(sockets.datagram_broadcast(listener, 1), -1);
        assert!(sockets.close(listener));
    }

    #[test]
    fn addresses_and_stream_options_preserve_leased_half_closed_handles() {
        for binding in ["127.0.0.1:0", "[::1]:0"] {
            let sockets = Sockets::default();
            let listener = sockets.listen(binding);
            let address = sockets.address(listener, false).unwrap();
            assert!(sockets.address(listener, true).is_none());
            assert_eq!(sockets.configure_stream(listener, |_| Ok(())), -1);
            let mut peer = TcpStream::connect(address).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            // Client connect completion does not guarantee that a nonblocking
            // listener is already readable. Follow the source accept loop,
            // retaining its handle until each one-shot registration retires.
            let listener_lease = sockets.get(listener).unwrap();
            let reactor = Reactor::new().unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let accepted = loop {
                let remaining = deadline
                    .checked_duration_since(std::time::Instant::now())
                    .expect("listener did not become readable");
                // SAFETY: listener_lease outlives the registration and reactor.
                let registration = unsafe {
                    reactor
                        .register(listener_lease.source(i64::from(READABLE)).unwrap(), 1)
                        .unwrap()
                };
                reactor.wait(Some(remaining)).unwrap();
                let ready = reactor.pop_ready().expect("listener readiness timed out");
                assert_eq!(ready.registration, registration);
                let accepted = sockets.accept(listener);
                if accepted != -2 {
                    break accepted;
                }
            };
            drop(reactor);
            drop(listener_lease);
            assert!(
                accepted > listener,
                "accept returned {accepted} on {binding}"
            );
            assert_eq!(sockets.address(accepted, false), Some(address));
            assert_eq!(
                sockets.address(accepted, true),
                Some(peer.local_addr().unwrap())
            );
            assert_eq!(sockets.local_port(accepted), i64::from(address.port()));
            let lease = sockets.get(accepted).unwrap();
            let Socket::Stream { stream, .. } = lease.as_ref() else {
                unreachable!()
            };
            for enabled in [true, false] {
                assert_eq!(
                    sockets.configure_stream(accepted, |stream| stream.set_nodelay(enabled)),
                    0
                );
                assert_eq!(stream.nodelay().unwrap(), enabled);
            }
            assert_eq!(sockets.keepalive(listener, 1, 60, 5, 3), -1);
            assert_eq!(sockets.keepalive(accepted, 1, 60, 5, 3), 0);
            let options = SockRef::from(stream);
            assert!(options.keepalive().unwrap());
            assert_eq!(options.tcp_keepalive_retries().unwrap(), 3);
            #[cfg(unix)]
            {
                assert_eq!(
                    options.tcp_keepalive_time().unwrap(),
                    Duration::from_secs(60)
                );
                assert_eq!(
                    options.tcp_keepalive_interval().unwrap(),
                    Duration::from_secs(5)
                );
            }
            for (enabled, idle, interval, retries) in [
                (2, 60, 5, 3),
                (1, 0, 5, 3),
                (1, 60, -1, 3),
                (1, 60, 5, 0),
                (1, i64::MAX, 5, 3),
                (1, 60, 5, i64::MAX),
            ] {
                assert_eq!(
                    sockets.keepalive(accepted, enabled, idle, interval, retries),
                    -1
                );
                assert!(options.keepalive().unwrap());
                assert_eq!(options.tcp_keepalive_retries().unwrap(), 3);
            }
            assert_eq!(sockets.keepalive(accepted, 0, 0, 0, 0), 0);
            assert!(!options.keepalive().unwrap());
            assert!(!sockets.close(accepted));
            assert_eq!(
                sockets.configure_stream(accepted, |stream| stream.shutdown(Shutdown::Write)),
                0
            );
            assert_eq!(peer.read(&mut [0]).unwrap(), 0);
            assert!(sockets.address(accepted, true).is_some());
            drop(lease);
            assert!(sockets.close(accepted));
            assert!(sockets.address(accepted, false).is_none());
            assert_eq!(sockets.configure_stream(accepted, |_| Ok(())), -1);
            assert_eq!(sockets.keepalive(accepted, 1, 60, 5, 3), -1);
            assert!(sockets.close(listener));
        }
    }

    #[test]
    fn refused_connect_notifications_remain_terminal_after_error_reads() {
        const CHILD: &str = "LOOM_TEST_REFUSED_CONNECT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Parallel process tests can temporarily inherit the listener
            // across fork, keeping it alive after this test drops its handle.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tasks::socket::tests::refused_connect_notifications_remain_terminal_after_error_reads",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }
        for address in ["127.0.0.1:0", "[::1]:0"] {
            let listener = TcpListener::bind(address).unwrap();
            let address = listener.local_addr().unwrap();
            // Keep an accepted connection alive after closing the listener.
            // The port stays reserved but rejects new connections; otherwise
            // a parallel test can reuse it and make this connect succeed.
            let _peer = TcpStream::connect(address).unwrap();
            let (_reservation, _) = listener.accept().unwrap();
            drop(listener);
            let sockets = Sockets::default();
            let token = sockets.connect(&address.to_string());
            if token < 0 {
                continue; // Some platforms report refusal immediately.
            }
            let socket = sockets.get(token).unwrap();
            let reactor = Reactor::new().unwrap();
            let registration = unsafe {
                reactor
                    .register(socket.source(i64::from(WRITABLE)).unwrap(), 1)
                    .unwrap()
            };
            reactor.wait(Some(Duration::from_secs(5))).unwrap();
            let ready = reactor
                .pop_ready()
                .expect("refused connection did not complete");
            assert_eq!(ready.registration, registration);
            socket.ready(ready.events);
            assert_eq!(sockets.connect_status(token), -1);
            assert_eq!(sockets.connect_status(token), -1);
            drop(socket);
            assert!(sockets.close(token));
        }
    }
}
