//! Host resource and readiness-handle adaptation.
//!
//! The scheduler exposes monotonic 64-bit capability tokens for File/Socket
//! values. Native descriptors remain inside concrete Rust owners in the task
//! ledger; reactor wait handles stay private to this module.

use std::fs::File;
use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
#[cfg(windows)]
use std::os::windows::io::{AsRawSocket, AsSocket, BorrowedSocket, OwnedSocket, RawSocket};

/// The all-ones ABI value is reserved and never allocated as a live resource
/// capability token.
pub(crate) const INVALID_RESOURCE_TOKEN: i64 = -1;

#[cfg(unix)]
pub(crate) type OwnedWaitHandle = OwnedFd;
#[cfg(windows)]
pub(crate) type OwnedWaitHandle = OwnedSocket;

#[cfg(unix)]
#[derive(Clone, Copy)]
pub(crate) struct RawPollSource(RawFd);

#[cfg(windows)]
#[derive(Clone, Copy)]
pub(crate) struct RawPollSource(RawSocket);

#[cfg(unix)]
impl AsRawFd for RawPollSource {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

#[cfg(unix)]
impl AsFd for RawPollSource {
    fn as_fd(&self) -> BorrowedFd<'_> {
        // SAFETY: RawPollSource is borrowed only while the matching runtime
        // registration keeps the caller-owned descriptor alive.
        unsafe { BorrowedFd::borrow_raw(self.0) }
    }
}

#[cfg(windows)]
impl AsRawSocket for RawPollSource {
    fn as_raw_socket(&self) -> RawSocket {
        self.0
    }
}

#[cfg(windows)]
impl AsSocket for RawPollSource {
    fn as_socket(&self) -> std::os::windows::io::BorrowedSocket<'_> {
        // SAFETY: RawPollSource is borrowed only while the matching runtime
        // registration keeps the caller-owned socket alive.
        unsafe { BorrowedSocket::borrow_raw(self.0) }
    }
}

#[cfg(unix)]
pub(crate) fn raw_poll_source(handle: i64) -> Option<RawPollSource> {
    i32::try_from(handle)
        .ok()
        .filter(|descriptor| *descriptor >= 0)
        .map(RawPollSource)
}

#[cfg(windows)]
pub(crate) fn raw_poll_source(handle: i64) -> Option<RawPollSource> {
    let bits = handle.cast_unsigned();
    (bits != u64::MAX)
        .then(|| RawSocket::try_from(bits).ok())
        .flatten()
        .map(RawPollSource)
}

#[cfg(unix)]
pub(crate) fn clone_wait_handle(source: &impl AsFd) -> io::Result<OwnedWaitHandle> {
    source.as_fd().try_clone_to_owned()
}

#[cfg(windows)]
pub(crate) fn clone_wait_handle(source: &impl AsSocket) -> io::Result<OwnedWaitHandle> {
    source.as_socket().try_clone_to_owned()
}

#[cfg(unix)]
pub(crate) fn wait_handle_bits(handle: &OwnedWaitHandle) -> i64 {
    i64::from(handle.as_raw_fd())
}

#[cfg(windows)]
pub(crate) fn wait_handle_bits(handle: &OwnedWaitHandle) -> i64 {
    handle.as_raw_socket().cast_signed()
}

/// A runtime-owned external resource paired with one unforgeable source token.
///
/// Generated code sees only `token`; every operation resolves it back to this
/// concrete owner in the active Task ledger before cloning or closing it.
pub(crate) struct OwnedResource {
    token: i64,
    owner: ResourceOwner,
}

enum ResourceOwner {
    File(File),
    Socket(TcpStream),
}

impl OwnedResource {
    pub(crate) fn token(&self) -> i64 {
        self.token
    }

    pub(crate) fn is_file(&self) -> bool {
        matches!(self.owner, ResourceOwner::File(_))
    }

    pub(crate) fn try_clone_file(&self) -> io::Result<File> {
        match &self.owner {
            ResourceOwner::File(file) => file.try_clone(),
            ResourceOwner::Socket(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "resource capability is not a File",
            )),
        }
    }

    pub(crate) fn try_clone_socket(&self) -> io::Result<TcpStream> {
        match &self.owner {
            ResourceOwner::Socket(socket) => socket.try_clone(),
            ResourceOwner::File(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "resource capability is not a Socket",
            )),
        }
    }
}

impl From<File> for OwnedResource {
    fn from(file: File) -> Self {
        Self {
            token: next_resource_token(),
            owner: ResourceOwner::File(file),
        }
    }
}

impl From<TcpStream> for OwnedResource {
    fn from(socket: TcpStream) -> Self {
        Self {
            token: next_resource_token(),
            owner: ResourceOwner::Socket(socket),
        }
    }
}

fn next_resource_token() -> i64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, advance_resource_token)
        .unwrap_or_else(|_| std::process::abort())
        .cast_signed()
}

const fn advance_resource_token(current: u64) -> Option<u64> {
    current.checked_add(1)
}

#[cfg(unix)]
pub(crate) fn socket_handle_bits(socket: &TcpStream) -> i64 {
    i64::from(socket.as_raw_fd())
}

#[cfg(windows)]
pub(crate) fn socket_handle_bits(socket: &TcpStream) -> i64 {
    socket.as_raw_socket().cast_signed()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn resource_token_sequence_never_wraps_or_allocates_the_sentinel() {
        assert_eq!(advance_resource_token(1), Some(2));
        assert_eq!(advance_resource_token(u64::MAX - 1), Some(u64::MAX));
        assert_eq!(advance_resource_token(u64::MAX), None);
    }

    #[test]
    fn concurrent_resource_tokens_are_unique() {
        let workers = (0..8)
            .map(|_| {
                std::thread::spawn(|| (0..128).map(|_| next_resource_token()).collect::<Vec<_>>())
            })
            .collect::<Vec<_>>();
        let tokens = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(tokens.len(), 1024);
        assert!(tokens.iter().all(|token| *token != INVALID_RESOURCE_TOKEN));
        assert_eq!(tokens.iter().copied().collect::<BTreeSet<_>>().len(), 1024);
    }
}
