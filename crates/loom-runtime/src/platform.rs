//! Host resource and readiness-handle adaptation.
//!
//! The runtime ABI carries opaque 64-bit handle bits. Unix descriptors and
//! Windows HANDLE/SOCKET values are converted only in this module, keeping
//! platform ownership types out of the scheduler and public wait ABI.

use std::fs::File;
use std::io;
use std::net::TcpStream;

#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
#[cfg(windows)]
use std::os::windows::io::{
    AsRawHandle, AsRawSocket, AsSocket, BorrowedHandle, BorrowedSocket, FromRawHandle,
    FromRawSocket, OwnedSocket, RawHandle, RawSocket,
};

/// The all-ones ABI value is never a live resource and represents a closed or
/// absent handle on every supported 64-bit host.
pub(crate) const INVALID_HANDLE: i64 = -1;

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

/// A runtime-owned external resource. Keeping the concrete standard-library
/// owner avoids reconstructing borrowed File/TcpStream values around every I/O
/// operation and makes Windows HANDLE versus SOCKET ownership explicit.
pub(crate) enum OwnedResource {
    File(File),
    Socket(TcpStream),
}

impl OwnedResource {
    pub(crate) fn handle_bits(&self) -> i64 {
        match self {
            Self::File(file) => file_handle_bits(file),
            Self::Socket(socket) => socket_handle_bits(socket),
        }
    }

    pub(crate) fn is_file(&self) -> bool {
        matches!(self, Self::File(_))
    }
}

impl From<File> for OwnedResource {
    fn from(file: File) -> Self {
        Self::File(file)
    }
}

impl From<TcpStream> for OwnedResource {
    fn from(socket: TcpStream) -> Self {
        Self::Socket(socket)
    }
}

#[cfg(unix)]
fn file_handle_bits(file: &File) -> i64 {
    i64::from(file.as_raw_fd())
}

#[cfg(windows)]
fn file_handle_bits(file: &File) -> i64 {
    u64::try_from(file.as_raw_handle() as usize)
        .expect("Windows HANDLE width cannot exceed the Loom 64-bit resource ABI")
        .cast_signed()
}

#[cfg(unix)]
pub(crate) fn socket_handle_bits(socket: &TcpStream) -> i64 {
    i64::from(socket.as_raw_fd())
}

#[cfg(windows)]
pub(crate) fn socket_handle_bits(socket: &TcpStream) -> i64 {
    socket.as_raw_socket().cast_signed()
}

#[cfg(unix)]
pub(crate) fn duplicate_file(handle: i64) -> io::Result<File> {
    let descriptor = raw_fd(handle)?;
    // SAFETY: the scoped Loom File owns this live descriptor for the duration
    // of the clone operation.
    unsafe { BorrowedFd::borrow_raw(descriptor) }
        .try_clone_to_owned()
        .map(File::from)
}

#[cfg(windows)]
pub(crate) fn duplicate_file(handle: i64) -> io::Result<File> {
    let handle = raw_handle(handle)?;
    // SAFETY: the scoped Loom File owns this live handle for the duration of
    // the clone operation.
    unsafe { BorrowedHandle::borrow_raw(handle) }
        .try_clone_to_owned()
        .map(File::from)
}

#[cfg(unix)]
pub(crate) fn duplicate_socket(handle: i64) -> io::Result<TcpStream> {
    let descriptor = raw_fd(handle)?;
    // SAFETY: the scoped Loom Socket owns this live descriptor for the
    // duration of the clone operation.
    unsafe { BorrowedFd::borrow_raw(descriptor) }
        .try_clone_to_owned()
        .map(TcpStream::from)
}

#[cfg(windows)]
pub(crate) fn duplicate_socket(handle: i64) -> io::Result<TcpStream> {
    let socket = raw_socket(handle)?;
    // SAFETY: the scoped Loom Socket owns this live SOCKET for the duration of
    // the clone operation.
    unsafe { BorrowedSocket::borrow_raw(socket) }
        .try_clone_to_owned()
        .map(TcpStream::from)
}

#[cfg(unix)]
fn raw_fd(handle: i64) -> io::Result<RawFd> {
    i32::try_from(handle)
        .ok()
        .filter(|descriptor| *descriptor >= 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid Unix descriptor"))
}

#[cfg(windows)]
fn raw_handle(handle: i64) -> io::Result<RawHandle> {
    let bits = handle.cast_unsigned();
    if bits == 0 || bits == u64::MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Windows handle",
        ));
    }
    usize::try_from(bits)
        .map(|value| value as RawHandle)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Windows handle is too wide"))
}

#[cfg(windows)]
fn raw_socket(handle: i64) -> io::Result<RawSocket> {
    let bits = handle.cast_unsigned();
    if bits == u64::MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Windows socket",
        ));
    }
    RawSocket::try_from(bits)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Windows socket is too wide"))
}

/// Closes a resource which has left scheduler ownership but still obeys the
/// compiler-private File/Socket ABI.
pub(crate) unsafe fn close_untracked(handle: i64, file: bool) -> io::Result<()> {
    if file {
        close_untracked_file(handle)
    } else {
        close_untracked_socket(handle)
    }
}

#[cfg(unix)]
fn close_untracked_file(handle: i64) -> io::Result<()> {
    let descriptor = raw_fd(handle)?;
    // SAFETY: the caller proved that this resource has left every runtime
    // ownership table and still owns its raw descriptor.
    drop(unsafe { File::from_raw_fd(descriptor) });
    Ok(())
}

#[cfg(windows)]
fn close_untracked_file(handle: i64) -> io::Result<()> {
    let handle = raw_handle(handle)?;
    // SAFETY: the caller proved that this resource has left every runtime
    // ownership table and still owns its raw HANDLE.
    drop(unsafe { File::from_raw_handle(handle) });
    Ok(())
}

#[cfg(unix)]
fn close_untracked_socket(handle: i64) -> io::Result<()> {
    let descriptor = raw_fd(handle)?;
    // SAFETY: the caller proved that this resource has left every runtime
    // ownership table and still owns its raw socket descriptor.
    drop(unsafe { TcpStream::from_raw_fd(descriptor) });
    Ok(())
}

#[cfg(windows)]
fn close_untracked_socket(handle: i64) -> io::Result<()> {
    let socket = raw_socket(handle)?;
    // SAFETY: the caller proved that this resource has left every runtime
    // ownership table and still owns its raw SOCKET.
    drop(unsafe { TcpStream::from_raw_socket(socket) });
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn raw_file_handle_rejects_both_windows_sentinel_values() {
        assert_eq!(
            raw_handle(0)
                .expect_err("NULL is not an owned Windows HANDLE")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            raw_handle(INVALID_HANDLE)
                .expect_err("all-ones is not an owned Windows HANDLE")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!raw_handle(1).expect("non-null HANDLE bits").is_null());
    }
}
