//! Exact process-output boundary for compiler-generated harness text.

use std::io::{self, Write};
#[cfg(unix)]
use std::sync::OnceLock;

#[cfg(any(unix, windows))]
use std::fs::File;
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(windows)]
use std::os::windows::io::AsHandle;

use loom_runtime_abi::{
    STDOUT_WRITE_FAILED, STDOUT_WRITE_INVALID_ARGUMENT, STDOUT_WRITE_OK, TYPED_LOG_OK,
    TYPED_LOG_WRITE_FAILED,
};

fn write_exact(mut writer: impl Write, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(bytes).and_then(|()| writer.flush())
}

#[cfg(unix)]
fn pipe_failures_are_reportable() -> bool {
    static SIGPIPE_IGNORED: OnceLock<bool> = OnceLock::new();
    *SIGPIPE_IGNORED.get_or_init(|| {
        // SAFETY: installing SIG_IGN is process-global but uses no borrowed
        // state. Loom defines a broken process-output pipe as an I/O failure
        // instead of allowing SIGPIPE to terminate generated entry points.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) != libc::SIG_ERR }
    })
}

#[cfg(unix)]
fn write_locked_stream(lock: &mut (impl Write + AsFd), bytes: &[u8]) -> io::Result<()> {
    lock.flush()?;
    let stream = lock.as_fd().try_clone_to_owned().map(File::from)?;
    write_exact(stream, bytes)
}

#[cfg(windows)]
fn write_locked_stream(lock: &mut (impl Write + AsHandle), bytes: &[u8]) -> io::Result<()> {
    lock.flush()?;
    let stream = lock.as_handle().try_clone_to_owned().map(File::from)?;
    write_exact(stream, bytes)
}

#[cfg(unix)]
/// Writes one exact byte range to standard output and flushes it.
pub fn write_process_stdout(bytes: &[u8]) -> i32 {
    if !pipe_failures_are_reportable() {
        return STDOUT_WRITE_FAILED;
    }
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    // File reports EBADF instead of applying std::io::Stdout's deliberate
    // missing-stream success policy. Keep the stdout lock live for the entire
    // raw write so distinct ABI calls cannot interleave through this process.
    write_locked_stream(&mut lock, bytes).map_or(STDOUT_WRITE_FAILED, |()| STDOUT_WRITE_OK)
}

#[cfg(windows)]
/// Writes one exact byte range to standard output and flushes it.
pub fn write_process_stdout(bytes: &[u8]) -> i32 {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    // The owned clone lets File issue raw WriteFile-backed writes without
    // transferring or closing the original process handle. Keep the standard
    // stdout lock live so distinct ABI calls cannot interleave.
    write_locked_stream(&mut lock, bytes).map_or(STDOUT_WRITE_FAILED, |()| STDOUT_WRITE_OK)
}

/// Writes one already-formatted log line to standard error while holding the
/// process-local Rust stderr lock for the complete raw write and flush.
///
/// This serializes Loom logging calls within one process. It does not claim
/// kernel-level atomicity against unrelated writers which bypass that lock.
#[cfg(unix)]
pub fn write_process_stderr(bytes: &[u8]) -> i32 {
    if !pipe_failures_are_reportable() {
        return TYPED_LOG_WRITE_FAILED;
    }
    let stderr = io::stderr();
    let mut lock = stderr.lock();
    write_locked_stream(&mut lock, bytes).map_or(TYPED_LOG_WRITE_FAILED, |()| TYPED_LOG_OK)
}

/// Windows counterpart of the exact, process-serialized stderr boundary.
#[cfg(windows)]
pub fn write_process_stderr(bytes: &[u8]) -> i32 {
    let stderr = io::stderr();
    let mut lock = stderr.lock();
    write_locked_stream(&mut lock, bytes).map_or(TYPED_LOG_WRITE_FAILED, |()| TYPED_LOG_OK)
}

/// Writes one exact byte range to standard output without C text-mode
/// translation, delimiter insertion, or NUL scanning.
#[unsafe(export_name = "loom_runtime_stdout_write_v1")]
pub unsafe extern "C" fn stdout_write_v1(data: *const u8, length: u64) -> i32 {
    let Ok(length) = usize::try_from(length) else {
        return STDOUT_WRITE_INVALID_ARGUMENT;
    };
    if length > isize::MAX as usize || (length != 0 && data.is_null()) {
        return STDOUT_WRITE_INVALID_ARGUMENT;
    }
    if length == 0 {
        return write_process_stdout(&[]);
    }
    // SAFETY: the private ABI requires a readable range for the complete
    // synchronous call; null and the maximum slice extent were checked above.
    let bytes = unsafe { std::slice::from_raw_parts(data, length) };
    write_process_stdout(bytes)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::process::{self, Command};
    use std::ptr;

    use super::*;

    struct AlwaysFails;
    struct FlushFails;
    const RAW_OUTPUT_CHILD_ENV: &str = "LOOM_RUNTIME_RAW_OUTPUT_TEST_CHILD";
    const BUFFERED_PREFIX: &str = "buffered-prefix:";
    const RAW_OUTPUT: &[u8] = b"loom-left\0loom-right\n";

    impl Write for AlwaysFails {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("deliberate output failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("deliberate output failure"))
        }
    }

    impl Write for FlushFails {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("deliberate flush failure"))
        }
    }

    #[test]
    fn exact_writer_preserves_nul_and_lf_and_reports_failures() {
        let mut output = Vec::new();
        assert!(write_exact(&mut output, b"left\0right\n").is_ok());
        assert_eq!(output, b"left\0right\n");
        assert!(write_exact(AlwaysFails, b"data").is_err());
        assert!(write_exact(FlushFails, b"data").is_err());
    }

    #[test]
    fn stdout_boundary_rejects_invalid_ranges_and_accepts_empty_null() {
        // SAFETY: these cases intentionally exercise inputs that are rejected
        // before the function attempts to form or read a slice.
        unsafe {
            assert_eq!(stdout_write_v1(ptr::null(), 0), STDOUT_WRITE_OK);
            assert_eq!(
                stdout_write_v1(ptr::null(), 1),
                STDOUT_WRITE_INVALID_ARGUMENT
            );
            assert_eq!(
                stdout_write_v1(ptr::dangling(), u64::MAX),
                STDOUT_WRITE_INVALID_ARGUMENT
            );
        }
    }

    #[test]
    fn exact_bytes_survive_child_stdout() {
        if std::env::var_os(RAW_OUTPUT_CHILD_ENV).is_some() {
            print!("{BUFFERED_PREFIX}");
            // SAFETY: RAW_OUTPUT is a process-lifetime readable byte range.
            let length = u64::try_from(RAW_OUTPUT.len()).expect("raw output length fits u64");
            let status = unsafe { stdout_write_v1(RAW_OUTPUT.as_ptr(), length) };
            process::exit(i32::from(status != STDOUT_WRITE_OK));
        }

        let output = Command::new(std::env::current_exe().expect("runtime test executable"))
            .args([
                "--exact",
                "output::tests::exact_bytes_survive_child_stdout",
                "--nocapture",
            ])
            .env(RAW_OUTPUT_CHILD_ENV, "1")
            .output()
            .expect("run raw-output child");
        assert!(output.status.success(), "{output:?}");
        let mut expected_suffix = BUFFERED_PREFIX.as_bytes().to_vec();
        expected_suffix.extend_from_slice(RAW_OUTPUT);
        assert!(
            output.stdout.ends_with(&expected_suffix),
            "raw stdout suffix changed: {:?}",
            output.stdout
        );
        assert!(!output.stdout.ends_with(b"loom-left\0loom-right\r\n"));
    }
}
