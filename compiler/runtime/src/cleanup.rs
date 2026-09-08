//! Synchronous lexical cleanup. Generated stack records remain live until pop;
//! fault draining does not unwind owner frames or remove their GC roots.

use std::cell::Cell;
use std::ptr;

type Callback = unsafe extern "C" fn(*mut u8);

#[repr(C)]
pub(super) struct Cleanup {
    previous: *mut Cleanup,
    callback: Callback,
    captures: *mut u8,
}

thread_local! {
    static HEAD: Cell<*mut Cleanup> = const { Cell::new(ptr::null_mut()) };
    static FIRST_FAULT: Cell<Option<(*const u8, usize)>> = const { Cell::new(None) };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_cleanup_push(
    record: *mut Cleanup,
    callback: Callback,
    captures: *mut u8,
) {
    // SAFETY: Generated code supplies one inactive, address-stable stack record.
    // Captures reference live authoritative local slots, not copied values.
    unsafe {
        record.write(Cleanup {
            previous: HEAD.get(),
            callback,
            captures,
        });
    }
    HEAD.set(record);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_cleanup_pop(record: *mut Cleanup) {
    if HEAD.get() != record {
        super::fatal("invalid cleanup registration order");
    }
    // SAFETY: The top record was initialized by push and its frame is live.
    HEAD.set(unsafe { (*record).previous });
}

fn write(mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let count = bytes.len().min(i32::MAX as usize);
        // SAFETY: The slice is readable for count bytes. libc uses the native
        // write ABI (including _write's unsigned count on Windows).
        let written = unsafe { libc::write(2, bytes.as_ptr().cast(), count as _) };
        if written <= 0 {
            return;
        }
        bytes = &bytes[written as usize..];
    }
}

pub(super) fn report(message: &[u8]) {
    // No formatting allocation, managed heap borrow, or handle-table lock.
    write(b"RuntimeFault: ");
    write(message);
    write(b"\n");
}

fn drain() {
    loop {
        let record = HEAD.get();
        if record.is_null() {
            return;
        }
        // Pop before invocation. If this callback faults, its recursive drain
        // sees only remaining registrations and preserves the first diagnostic.
        // No TLS/heap borrow is held while generated code executes.
        unsafe {
            let callback = (*record).callback;
            let captures = (*record).captures;
            HEAD.set((*record).previous);
            callback(captures);
        }
    }
}

pub(super) fn fault(message: &[u8]) -> ! {
    let first = FIRST_FAULT.get().unwrap_or_else(|| {
        let first = (message.as_ptr(), message.len());
        FIRST_FAULT.set(Some(first));
        first
    });
    drain();
    // User cleanup is finished. Ignore SIGPIPE only for terminal reporting so
    // a broken diagnostic pipe still yields the language-fault exit status.
    // This process-wide disposition need not be restored: no code resumes.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    // SAFETY: Diagnostic bytes are static or Rust-owned, never managed interior
    // pointers. The original faulting frame cannot return or unwind, including
    // when a cleanup faults recursively. Retain that first message until exit.
    report(unsafe { std::slice::from_raw_parts(first.0, first.1) });
    std::process::exit(1)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_fault(message: *const u8, length: usize) -> ! {
    // SAFETY: The compiler passes a nonempty static diagnostic byte string.
    fault(unsafe { std::slice::from_raw_parts(message, length) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    unsafe extern "C" fn increment(value: *mut u8) {
        // SAFETY: Test captures point to this thread's live Int storage.
        unsafe { *value.cast::<usize>() += 1 };
    }

    #[test]
    fn lexical_records_pop_before_invocation_and_can_be_reused() {
        let mut calls = 0usize;
        let captures = ptr::addr_of_mut!(calls).cast();
        let mut outer = MaybeUninit::<Cleanup>::uninit();
        let mut inner = MaybeUninit::<Cleanup>::uninit();
        unsafe {
            loom_rt_cleanup_push(outer.as_mut_ptr(), increment, captures);
            loom_rt_cleanup_push(inner.as_mut_ptr(), increment, captures);
            loom_rt_cleanup_pop(inner.as_mut_ptr());
            increment(captures);
            loom_rt_cleanup_push(inner.as_mut_ptr(), increment, captures);
        }
        drain();
        assert_eq!(calls, 3);
        assert!(HEAD.get().is_null());
        drain();
        assert_eq!(calls, 3);
    }

    #[test]
    #[cfg(unix)]
    fn fault_cleanup_precedes_terminal_diagnostics() {
        use std::fs::File;
        use std::os::fd::FromRawFd;
        use std::process::{Command, Stdio};

        const CHILD: &str = "LOOM_RUNTIME_CLEANUP_FAULT_CHILD";
        unsafe extern "C" fn outer(_: *mut u8) {
            write_stdout(b"outer.cleanup\n");
        }
        unsafe extern "C" fn inner(_: *mut u8) {
            write_stdout(b"inner.cleanup\n");
            fault(b"secondary");
        }
        fn write_stdout(bytes: &[u8]) {
            // SAFETY: The parent captures this child's stdout through an open pipe.
            assert_eq!(
                unsafe { libc::write(1, bytes.as_ptr().cast(), bytes.len()) },
                bytes.len() as isize
            );
        }

        if std::env::var_os(CHILD).is_some() {
            // Rust's test harness ignores SIGPIPE; native Loom starts with its
            // default disposition. Exercise that native behavior explicitly.
            unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
            let mut first = MaybeUninit::<Cleanup>::uninit();
            let mut second = MaybeUninit::<Cleanup>::uninit();
            unsafe {
                loom_rt_cleanup_push(first.as_mut_ptr(), outer, ptr::null_mut());
                loom_rt_cleanup_push(second.as_mut_ptr(), inner, ptr::null_mut());
            }
            let message = *b"primary";
            fault(&message);
        }

        for broken in [false, true] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "cleanup::tests::fault_cleanup_precedes_terminal_diagnostics",
                    "--nocapture",
                ])
                .env(CHILD, "1");
            if broken {
                let mut pipe = [-1; 2];
                // SAFETY: pipe initializes two distinct owned descriptors.
                assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
                let reader = unsafe { File::from_raw_fd(pipe[0]) };
                let writer = unsafe { File::from_raw_fd(pipe[1]) };
                drop(reader);
                child.stderr(Stdio::from(writer));
            }
            let output = child.output().unwrap();
            assert_eq!(output.status.code(), Some(1));
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(stdout.ends_with("inner.cleanup\nouter.cleanup\n"));
            assert_eq!(stdout.matches(".cleanup\n").count(), 2);
            if !broken {
                assert_eq!(output.stderr, b"RuntimeFault: primary\n");
            }
        }
    }
}
