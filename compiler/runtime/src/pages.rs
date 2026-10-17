//! Aligned GC page groups owned directly through mmap / VirtualAlloc.

use super::{PAGE_SIZE, fatal};
use std::ptr::NonNull;

pub(super) const PAGES_PER_MAPPING: usize = 16;
const SIZE: usize = PAGES_PER_MAPPING * PAGE_SIZE;

pub(super) struct Mapping {
    pub(super) pointer: NonNull<u8>,
    #[cfg(windows)]
    reservation: *mut std::ffi::c_void,
}

impl Mapping {
    #[cfg(unix)]
    pub(super) fn new() -> Self {
        // SAFETY: Anonymous private storage; no existing mapping is replaced.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                SIZE + PAGE_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            fatal("out of memory");
        }
        let base = NonNull::new(base.cast::<u8>()).unwrap_or_else(|| fatal("out of memory"));
        let prefix = base.as_ptr().addr().wrapping_neg() & (PAGE_SIZE - 1);
        // SAFETY: The aligned payload fits inside this padded reservation.
        let pointer = unsafe { base.add(prefix) };
        // All boundaries are OS-page aligned on the supported hosts. Trim only
        // this mapping's padding, preserving the aligned payload until Drop.
        if prefix > 0 {
            unsafe { release(base.as_ptr(), prefix) };
        }
        unsafe { release(pointer.as_ptr().add(SIZE), PAGE_SIZE - prefix) };
        Self { pointer }
    }

    #[cfg(windows)]
    pub(super) fn new() -> Self {
        const MEM_RESERVE: u32 = 0x2000;
        const MEM_COMMIT: u32 = 0x1000;
        const PAGE_NOACCESS: u32 = 0x01;
        const PAGE_READWRITE: u32 = 0x04;
        // Reserve padding without charging physical storage, then commit only
        // the aligned payload. Release must use the original reservation base.
        let reservation = unsafe {
            VirtualAlloc(
                std::ptr::null_mut(),
                SIZE + PAGE_SIZE,
                MEM_RESERVE,
                PAGE_NOACCESS,
            )
        };
        let base = NonNull::new(reservation.cast::<u8>()).unwrap_or_else(|| fatal("out of memory"));
        let prefix = base.as_ptr().addr().wrapping_neg() & (PAGE_SIZE - 1);
        // SAFETY: The aligned payload fits entirely within this reservation.
        let pointer = unsafe { base.add(prefix) };
        let committed =
            unsafe { VirtualAlloc(pointer.as_ptr().cast(), SIZE, MEM_COMMIT, PAGE_READWRITE) };
        if committed.is_null() {
            fatal("out of memory");
        }
        Self {
            pointer,
            reservation,
        }
    }
}

#[cfg(unix)]
unsafe fn release(pointer: *mut u8, size: usize) {
    // SAFETY: The caller owns exactly this page-aligned mapping range.
    if unsafe { libc::munmap(pointer.cast(), size) } != 0 {
        fatal("cannot release GC pages");
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualAlloc(
        address: *mut std::ffi::c_void,
        size: usize,
        kind: u32,
        protection: u32,
    ) -> *mut std::ffi::c_void;
    fn VirtualFree(address: *mut std::ffi::c_void, size: usize, kind: u32) -> i32;
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: Mapping is the unique owner; arena values expire before drop.
        #[cfg(unix)]
        unsafe {
            release(self.pointer.as_ptr(), SIZE);
        }
        #[cfg(windows)]
        {
            const MEM_RELEASE: u32 = 0x8000;
            if unsafe { VirtualFree(self.reservation, 0, MEM_RELEASE) } == 0 {
                fatal("cannot release GC pages");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_are_aligned_writable_and_independently_owned() {
        let first = Mapping::new();
        let second = Mapping::new();
        assert_eq!(first.pointer.as_ptr().addr() % PAGE_SIZE, 0);
        assert_eq!(second.pointer.as_ptr().addr() % PAGE_SIZE, 0);
        assert_ne!(first.pointer, second.pointer);
        // SAFETY: Each complete mapping is writable and remains uniquely owned.
        unsafe {
            std::ptr::write_bytes(first.pointer.as_ptr(), 0x5a, SIZE);
            std::ptr::write_bytes(second.pointer.as_ptr(), 0xa5, SIZE);
            assert_eq!(*first.pointer.as_ptr(), 0x5a);
            assert_eq!(*first.pointer.as_ptr().add(SIZE - 1), 0x5a);
            drop(first);
            assert_eq!(*second.pointer.as_ptr(), 0xa5);
            assert_eq!(*second.pointer.as_ptr().add(SIZE - 1), 0xa5);
        }
    }
}
