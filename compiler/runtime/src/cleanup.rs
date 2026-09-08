//! Lexical cleanup runs while its native captures and GC roots are still live.
//! Only an explicit resume boundary catches faults; other faults end the process.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::ptr;

type Callback = unsafe extern "C-unwind" fn(*mut u8);
type DiagnosticBytes = (*const u8, usize);
type BeforeDrain = (unsafe fn(*mut u8), *mut u8);

#[derive(Clone, Copy)]
struct Fault {
    message: DiagnosticBytes,
    test_name: Option<DiagnosticBytes>,
}

#[derive(Debug)]
pub(super) struct OwnedFault {
    pub message: Vec<u8>,
    pub test_name: Option<Vec<u8>>,
}

struct Boundary {
    previous: *const Boundary,
    cleanup: *mut Cleanup,
    roots: *mut super::RootFrame,
    first: RefCell<Option<OwnedFault>>,
    before_drain: Cell<Option<BeforeDrain>>,
}

// The diagnostic stays in its owner boundary, never in an unwound stack frame.
struct FaultUnwind;

#[repr(C)]
pub(super) struct Cleanup {
    previous: *mut Cleanup,
    callback: Callback,
    captures: *mut u8,
}

thread_local! {
    static HEAD: Cell<*mut Cleanup> = const { Cell::new(ptr::null_mut()) };
    static TEST_NAME: Cell<Option<DiagnosticBytes>> = const { Cell::new(None) };
    static FIRST_FAULT: Cell<Option<Fault>> = const { Cell::new(None) };
    static BOUNDARY: Cell<*const Boundary> = const { Cell::new(ptr::null()) };
}

/// Catch a language fault from one synchronous native resume activation.
///
/// # Safety
/// Calls may unwind only through Loom frames with unwind tables or Rust/C-unwind
/// frames. No foreign exception or independent catcher intercepts the fault.
/// All cleanup captures remain live until normal pop or fault draining. Root
/// frames inside this boundary are removed normally or collectively by fault;
/// they must not also leave from a Rust unwind destructor. The outer frame-root
/// owner scope stays outside this boundary. No heap borrow or GC tracer is active
/// on entry, and unwinding destructors cannot allocate Loom objects or call user
/// code after the root chain has been restored.
pub(super) unsafe fn catch_fault<R>(run: impl FnOnce() -> R) -> Result<R, OwnedFault> {
    // SAFETY: The caller supplies the native stack/unwind contract above.
    unsafe { catch_fault_inner(run, None) }
}

/// A task resume first retires descendants and its borrowed wait before native
/// cleanup can close resources. The hook runs once, after owning the first
/// diagnostic. It must catch its own cleanup faults and return normally.
///
/// # Safety
/// The catch_fault contract applies. Data stays live through this activation;
/// the hook holds no heap/root/task borrow while invoking generated cleanup.
pub(super) unsafe fn catch_fault_before_drain<R>(
    run: impl FnOnce() -> R,
    before_drain: unsafe fn(*mut u8),
    data: *mut u8,
) -> Result<R, OwnedFault> {
    // SAFETY: The caller retains data and supplies the hook contract above.
    unsafe { catch_fault_inner(run, Some((before_drain, data))) }
}

unsafe fn catch_fault_inner<R>(
    run: impl FnOnce() -> R,
    before_drain: Option<BeforeDrain>,
) -> Result<R, OwnedFault> {
    let roots = super::HEAP.with(|heap| {
        let heap = heap.borrow();
        if heap.collecting {
            super::fatal("fault boundary during GC tracing");
        }
        heap.roots
    });
    let boundary = Boundary {
        previous: BOUNDARY.get(),
        cleanup: HEAD.get(),
        roots,
        first: RefCell::new(None),
        before_drain: Cell::new(before_drain),
    };
    let test_name = TEST_NAME.get();
    let first_fault = FIRST_FAULT.replace(None);
    // This shared address stays fixed through catch_unwind. Only the RefCell
    // payload changes, and no borrow spans a generated callback.
    BOUNDARY.set(ptr::from_ref(&boundary));
    let result = catch_unwind(AssertUnwindSafe(run));
    BOUNDARY.set(boundary.previous);
    TEST_NAME.set(test_name);
    FIRST_FAULT.set(first_fault);
    if let Err(payload) = &result {
        if !payload.is::<FaultUnwind>() || boundary.first.borrow().is_none() {
            // Arbitrary Rust panics may have bypassed the live-stack drain.
            // Never inspect stale records or treat runtime bugs as TaskFault.
            super::fatal("unexpected Rust panic at fault boundary");
        }
    }
    if HEAD.get() != boundary.cleanup
        || super::HEAP.with(|heap| heap.borrow().roots) != boundary.roots
    {
        // Normal return must balance registrations itself. The creator's stack
        // is already gone here, so attempting a late drain would be invalid.
        super::fatal("unbalanced fault boundary registrations");
    }
    match result {
        Ok(value) => {
            if boundary.first.borrow().is_some() {
                super::fatal("fault escaped its resume boundary");
            }
            Ok(value)
        }
        Err(_) => Err(boundary.first.into_inner().expect("recorded runtime fault")),
    }
}

// Only the generated test entry calls these, once around each selected test.
// Ordinary functions and executable entries have no test instrumentation.
#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_test_enter(name: *const u8, length: usize) {
    // Generated labels are static UTF-8 bytes, never managed heap pointers.
    TEST_NAME.set(Some((name, length)));
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_test_leave() {
    TEST_NAME.set(None);
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

fn drain_to(boundary: *mut Cleanup) {
    loop {
        let record = HEAD.get();
        if record == boundary {
            return;
        }
        if record.is_null() {
            super::fatal("missing cleanup boundary");
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
    let boundary = BOUNDARY.get();
    if !boundary.is_null() {
        // SAFETY: catch_fault's shared stack boundary stays live until its own
        // catcher receives this private unwind. Nested boundaries restore it.
        let boundary = unsafe { &*boundary };
        {
            let mut first = boundary.first.borrow_mut();
            if first.is_none() {
                *first = Some(OwnedFault {
                    message: message.to_vec(),
                    test_name: TEST_NAME.get().map(|(name, length)| {
                        // SAFETY: Generated test labels are static native bytes.
                        unsafe { std::slice::from_raw_parts(name, length) }.to_vec()
                    }),
                });
            }
        }
        if let Some((before_drain, data)) = boundary.before_drain.take() {
            // SAFETY: The resume owner remains live and the hook catches its
            // own cleanup faults. Take first so recursive faults cannot repeat it.
            unsafe { before_drain(data) };
        }
        drain_to(boundary.cleanup);
        // Every user cleanup has finished while its root slots were valid.
        // Cut the abandoned chain before unwinding makes those stack addresses
        // stale; no generated callback or Loom allocation runs after this point.
        super::HEAP.with(|heap| {
            let mut heap = heap.borrow_mut();
            if heap.collecting {
                super::fatal("runtime fault during GC tracing");
            }
            heap.roots = boundary.roots;
        });
        // resume_unwind deliberately bypasses the process-wide Rust panic hook.
        resume_unwind(Box::new(FaultUnwind));
    }
    let first = FIRST_FAULT.get().unwrap_or_else(|| {
        let first = Fault {
            message: (message.as_ptr(), message.len()),
            test_name: TEST_NAME.get(),
        };
        FIRST_FAULT.set(Some(first));
        first
    });
    drain_to(ptr::null_mut());
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
    if let Some((name, length)) = first.test_name {
        write(b"FAIL ");
        // SAFETY: Test labels are static and snapshotted before any cleanup.
        write(unsafe { std::slice::from_raw_parts(name, length) });
        write(b"\n");
    }
    report(unsafe { std::slice::from_raw_parts(first.message.0, first.message.1) });
    std::process::exit(1)
}

pub(super) fn raise_owned(failure: OwnedFault) -> ! {
    // These Rust-owned bytes remain live until the live-stack drain finishes.
    // A catcher copies them before unwind and restores the previous test label.
    TEST_NAME.set(
        failure
            .test_name
            .as_ref()
            .map(|name| (name.as_ptr(), name.len())),
    );
    fault(&failure.message)
}

#[unsafe(no_mangle)]
unsafe extern "C-unwind" fn loom_rt_fault(message: *const u8, length: usize) -> ! {
    // SAFETY: The compiler passes a nonempty static diagnostic byte string.
    fault(unsafe { std::slice::from_raw_parts(message, length) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    unsafe fn before_drain_order(data: *mut u8) {
        unsafe { (*data.cast::<Vec<u8>>()).push(1) };
        let secondary = unsafe { catch_fault(|| fault(b"hook cleanup fault")) }.unwrap_err();
        assert_eq!(secondary.message, b"hook cleanup fault");
    }

    unsafe extern "C-unwind" fn faulting_order(data: *mut u8) {
        unsafe { (*data.cast::<Vec<u8>>()).push(2) };
        fault(b"native cleanup fault");
    }

    unsafe extern "C-unwind" fn final_order(data: *mut u8) {
        unsafe { (*data.cast::<Vec<u8>>()).push(3) };
    }

    #[test]
    fn task_hook_runs_once_before_native_drain_with_first_diagnostic_owned() {
        let mut order = Vec::<u8>::new();
        let data = ptr::addr_of_mut!(order).cast();
        let failure = unsafe {
            catch_fault_before_drain(
                || {
                    let mut outer = MaybeUninit::uninit();
                    let mut inner = MaybeUninit::uninit();
                    loom_rt_cleanup_push(outer.as_mut_ptr(), final_order, data);
                    loom_rt_cleanup_push(inner.as_mut_ptr(), faulting_order, data);
                    fault(b"activation fault");
                },
                before_drain_order,
                data,
            )
        }
        .unwrap_err();
        assert_eq!(failure.message, b"activation fault");
        assert_eq!(order, [1, 2, 3]);
    }

    unsafe extern "C-unwind" fn increment(value: *mut u8) {
        // SAFETY: Test captures point to this thread's live Int storage.
        unsafe { *value.cast::<usize>() += 1 };
    }

    #[test]
    fn lexical_records_pop_before_invocation_and_can_be_reused() {
        unsafe { loom_rt_test_enter(b"package.test".as_ptr(), 12) };
        assert_eq!(TEST_NAME.get().unwrap().1, 12);
        loom_rt_test_leave();
        assert!(TEST_NAME.get().is_none());
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
        drain_to(ptr::null_mut());
        assert_eq!(calls, 3);
        assert!(HEAD.get().is_null());
        drain_to(ptr::null_mut());
        assert_eq!(calls, 3);
    }

    #[test]
    fn nested_fault_boundaries_preserve_outer_cleanup_and_context() {
        let mut outside_calls = 0usize;
        let mut outer_calls = 0usize;
        let mut inner_calls = 0usize;
        let mut outside = MaybeUninit::uninit();
        let outside_record = outside.as_mut_ptr();
        // SAFETY: Captures and stack records outlive every pop/drain. Both
        // nested boundaries call only Rust or C-unwind functions.
        unsafe {
            loom_rt_test_enter(b"outside".as_ptr(), 7);
            loom_rt_cleanup_push(
                outside_record,
                increment,
                ptr::addr_of_mut!(outside_calls).cast(),
            );
            let value: Result<(), _> = catch_fault(|| {
                loom_rt_test_enter(b"outer".as_ptr(), 5);
                let mut outer = MaybeUninit::uninit();
                loom_rt_cleanup_push(
                    outer.as_mut_ptr(),
                    increment,
                    ptr::addr_of_mut!(outer_calls).cast(),
                );
                let inner: Result<(), _> = catch_fault(|| {
                    loom_rt_test_enter(b"inner".as_ptr(), 5);
                    let mut inner = MaybeUninit::uninit();
                    loom_rt_cleanup_push(
                        inner.as_mut_ptr(),
                        increment,
                        ptr::addr_of_mut!(inner_calls).cast(),
                    );
                    let message = Vec::from(b"owned inner fault");
                    fault(&message);
                });
                let inner = inner.unwrap_err();
                assert_eq!(inner.message, b"owned inner fault");
                assert_eq!(inner.test_name.as_deref(), Some(b"inner".as_slice()));
                assert_eq!(inner_calls, 1);
                assert_eq!(outer_calls, 0);
                let (name, length) = TEST_NAME.get().unwrap();
                assert_eq!(std::slice::from_raw_parts(name, length), b"outer");
                fault(b"outer fault");
            });
            let fault: OwnedFault = value.unwrap_err();
            assert_eq!(fault.message, b"outer fault");
            assert_eq!(fault.test_name.as_deref(), Some(b"outer".as_slice()));
            assert_eq!(outside_calls, 0);
            assert_eq!(outer_calls, 1);
            assert_eq!(inner_calls, 1);
            assert_eq!(HEAD.get(), outside_record);
            assert!(BOUNDARY.get().is_null());
            assert!(FIRST_FAULT.get().is_none());
            let (name, length) = TEST_NAME.get().unwrap();
            assert_eq!(std::slice::from_raw_parts(name, length), b"outside");
            assert_eq!(catch_fault(|| 42).unwrap(), 42);
            loom_rt_cleanup_pop(outside_record);
            increment(ptr::addr_of_mut!(outside_calls).cast());
            loom_rt_test_leave();
        }
        assert_eq!(outside_calls, 1);
    }

    #[test]
    fn recursive_fault_cleanup_keeps_live_roots_until_drain_finishes() {
        use crate::{HEAP, loom_rt_collect, loom_rt_text_new, rooted, text_bytes};

        struct Captures {
            text: *mut *mut u8,
            events: *mut Vec<u8>,
        }
        unsafe extern "C-unwind" fn inspect_and_collect(address: *mut u8) {
            // SAFETY: This native capture and authoritative root slot are in
            // the faulting activation, still live throughout synchronous drain.
            unsafe {
                let captures = &*address.cast::<Captures>();
                (*captures.events).push(2);
                loom_rt_collect();
                assert_eq!(text_bytes(*captures.text), b"retained during cleanup");
            }
        }
        unsafe extern "C-unwind" fn secondary(address: *mut u8) {
            unsafe { (*(*address.cast::<Captures>()).events).push(1) };
            loom_rt_test_leave();
            fault(b"secondary");
        }

        loom_rt_collect();
        let baseline_roots = HEAP.with(|heap| heap.borrow().roots);
        let baseline_objects = HEAP.with(|heap| heap.borrow().objects.len());
        let mut events = Vec::new();
        // SAFETY: rooted() only pops on normal return, so this boundary owns
        // the collective root-head rollback. No unwind destructor calls Loom.
        let failure: Result<(), _> = unsafe {
            catch_fault(|| {
                loom_rt_test_enter(b"primary.test".as_ptr(), 12);
                rooted([ptr::null_mut()], |slots| {
                    *slots = loom_rt_text_new(b"retained during cleanup".as_ptr(), 23);
                    let captures = Captures {
                        text: slots,
                        events: ptr::addr_of_mut!(events),
                    };
                    let address = ptr::from_ref(&captures).cast_mut().cast();
                    let mut outer = MaybeUninit::uninit();
                    let mut inner = MaybeUninit::uninit();
                    loom_rt_cleanup_push(outer.as_mut_ptr(), inspect_and_collect, address);
                    loom_rt_cleanup_push(inner.as_mut_ptr(), secondary, address);
                    let message = Vec::from(b"primary");
                    fault(&message);
                });
            })
        };
        let failure = failure.unwrap_err();
        assert_eq!(failure.message, b"primary");
        assert_eq!(
            failure.test_name.as_deref(),
            Some(b"primary.test".as_slice())
        );
        assert_eq!(events, [1, 2]);
        assert_eq!(HEAP.with(|heap| heap.borrow().roots), baseline_roots);
        assert!(HEAD.get().is_null());
        assert!(FIRST_FAULT.get().is_none());
        assert!(TEST_NAME.get().is_none());
        loom_rt_collect();
        assert_eq!(
            HEAP.with(|heap| heap.borrow().objects.len()),
            baseline_objects
        );
    }

    #[test]
    #[cfg(unix)]
    fn fault_cleanup_precedes_terminal_diagnostics() {
        use std::fs::File;
        use std::os::fd::FromRawFd;
        use std::process::{Command, Stdio};

        const CHILD: &str = "LOOM_RUNTIME_CLEANUP_FAULT_CHILD";
        unsafe extern "C-unwind" fn outer(_: *mut u8) {
            write_stdout(b"outer.cleanup\n");
        }
        unsafe extern "C-unwind" fn inner(_: *mut u8) {
            write_stdout(b"inner.cleanup\n");
            // Even a host callback changing context cannot relabel the first fault.
            loom_rt_test_leave();
            fault(b"secondary");
        }
        fn write_stdout(bytes: &[u8]) {
            // SAFETY: The parent captures this child's stdout through an open pipe.
            assert_eq!(
                unsafe { libc::write(1, bytes.as_ptr().cast(), bytes.len()) },
                bytes.len() as isize
            );
        }

        if let Some(mode) = std::env::var_os(CHILD) {
            // Rust's test harness ignores SIGPIPE; native Loom starts with its
            // default disposition. Exercise that native behavior explicitly.
            unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
            if mode == "named" {
                unsafe { loom_rt_test_enter(b"package.test".as_ptr(), 12) };
            }
            let mut first = MaybeUninit::<Cleanup>::uninit();
            let mut second = MaybeUninit::<Cleanup>::uninit();
            unsafe {
                loom_rt_cleanup_push(first.as_mut_ptr(), outer, ptr::null_mut());
                loom_rt_cleanup_push(second.as_mut_ptr(), inner, ptr::null_mut());
            }
            let message = *b"primary";
            fault(&message);
        }

        for mode in ["plain", "named", "broken"] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "cleanup::tests::fault_cleanup_precedes_terminal_diagnostics",
                    "--nocapture",
                ])
                .env(CHILD, mode);
            if mode == "broken" {
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
            if mode != "broken" {
                let expected: &[u8] = if mode == "named" {
                    b"FAIL package.test\nRuntimeFault: primary\n"
                } else {
                    b"RuntimeFault: primary\n"
                };
                assert_eq!(output.stderr, expected);
            }
        }
    }
}
