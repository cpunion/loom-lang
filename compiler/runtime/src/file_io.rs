//! Synchronous file handles. Lifetime belongs to explicit source-level close,
//! never GC; Windows file slots avoid the CRT's text mode and ANSI paths.

#[cfg(unix)]
mod platform {
    pub fn open(path: &str, create: bool) -> i64 {
        let Ok(path) = std::ffi::CString::new(path) else {
            return -1;
        };
        // SAFETY: open consumes the NUL-terminated path without retaining it;
        // the process umask determines permissions when creating a file.
        i64::from(unsafe {
            if create {
                libc::open(
                    path.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                    0o666 as libc::c_uint,
                )
            } else {
                libc::open(path.as_ptr(), libc::O_RDONLY)
            }
        })
    }
    pub fn read(fd: i64, buffer: &mut [u8]) -> i64 {
        let Ok(fd) = libc::c_int::try_from(fd) else {
            return -1;
        };
        // SAFETY: The mutable slice is writable for the requested byte count.
        unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) as i64 }
    }
    pub fn write(fd: i64, bytes: &[u8]) -> i64 {
        let Ok(fd) = libc::c_int::try_from(fd) else {
            return -1;
        };
        // SAFETY: The immutable slice is readable for the requested byte count.
        unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) as i64 }
    }
    pub fn close(fd: i64) -> i64 {
        let Ok(fd) = libc::c_int::try_from(fd) else {
            return -1;
        };
        // SAFETY: close validates the descriptor; the source owns its lifetime.
        i64::from(unsafe { libc::close(fd) })
    }
}

#[cfg(windows)]
mod platform {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::fs::File;
    use std::io::{Read, Write};

    #[derive(Default)]
    struct Handles {
        files: Vec<Option<File>>,
        standard_closed: [bool; 3],
    }
    thread_local! { static HANDLES: RefCell<Handles> = RefCell::new(Handles::default()); }

    // Only explicit close of standard handles needs this OS boundary. Rust's
    // File and standard streams otherwise own wide paths and binary I/O.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    fn count(value: std::io::Result<usize>) -> i64 {
        value.map_or(-1, |value| value as i64)
    }
    pub fn open(path: &str, create: bool) -> i64 {
        let Ok(file) = (if create {
            File::create(path)
        } else {
            File::open(path)
        }) else {
            return -1;
        };
        HANDLES.with(|handles| {
            let mut handles = handles.borrow_mut();
            let slot = handles
                .files
                .iter()
                .position(Option::is_none)
                .unwrap_or(handles.files.len());
            if slot == handles.files.len() {
                handles.files.push(Some(file));
            } else {
                handles.files[slot] = Some(file);
            }
            slot as i64 + 3
        })
    }
    pub fn read(fd: i64, buffer: &mut [u8]) -> i64 {
        let Ok(fd) = usize::try_from(fd) else {
            return -1;
        };
        HANDLES.with(|handles| {
            let mut handles = handles.borrow_mut();
            if fd == 0 && !handles.standard_closed[0] {
                return count(std::io::stdin().lock().read(buffer));
            }
            if fd < 3 {
                return -1;
            }
            handles
                .files
                .get_mut(fd - 3)
                .and_then(Option::as_mut)
                .map_or(-1, |file| count(file.read(buffer)))
        })
    }
    fn output(mut stream: impl Write, bytes: &[u8]) -> i64 {
        match stream.write(bytes) {
            Ok(written) => {
                if stream.flush().is_ok() {
                    written as i64
                } else {
                    -1
                }
            }
            Err(_) => -1,
        }
    }
    pub fn write(fd: i64, bytes: &[u8]) -> i64 {
        let Ok(fd) = usize::try_from(fd) else {
            return -1;
        };
        HANDLES.with(|handles| {
            let mut handles = handles.borrow_mut();
            if fd < 3 {
                if handles.standard_closed[fd] {
                    return -1;
                }
                return match fd {
                    1 => output(std::io::stdout().lock(), bytes),
                    2 => output(std::io::stderr().lock(), bytes),
                    _ => -1,
                };
            }
            handles
                .files
                .get_mut(fd - 3)
                .and_then(Option::as_mut)
                .map_or(-1, |file| count(file.write(bytes)))
        })
    }
    pub fn close(fd: i64) -> i64 {
        let Ok(fd) = usize::try_from(fd) else {
            return -1;
        };
        HANDLES.with(|handles| {
            let mut handles = handles.borrow_mut();
            if fd < 3 {
                if handles.standard_closed[fd] {
                    return -1;
                }
                // SAFETY: GetStdHandle retrieves the process-owned standard
                // handle. CloseHandle validates it; no borrowed File is kept.
                let closed = unsafe { CloseHandle(GetStdHandle((-10i32 - fd as i32) as u32)) };
                if closed == 0 {
                    return -1;
                }
                handles.standard_closed[fd] = true;
                return 0;
            }
            if handles
                .files
                .get_mut(fd - 3)
                .and_then(Option::take)
                .is_some()
            {
                0
            } else {
                -1
            }
        })
    }
}

pub use platform::{close, open, read, write};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_paths_and_binary_bytes_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("目录-é-🙂.bin");
        let path = path.to_str().unwrap();
        let payload = "a\r\nb\u{1a}\0界🙂".as_bytes();
        let file = open(path, true);
        assert!(file >= 0);
        assert_eq!(write(file, payload), payload.len() as i64);
        assert_eq!(close(file), 0);
        assert_eq!(std::fs::read(path).unwrap(), payload);
        let file = open(path, false);
        let mut copied = Vec::new();
        let mut bytes = [0; 2];
        loop {
            let count = read(file, &mut bytes);
            assert!(count >= 0);
            if count == 0 {
                break;
            }
            copied.extend_from_slice(&bytes[..count as usize]);
        }
        assert_eq!(copied, payload);
        assert_eq!(close(file), 0);
        assert_eq!(read(-1, &mut bytes), -1);
        assert_eq!(write(i64::MAX, payload), -1);
        assert_eq!(close(-1), -1);
        assert_eq!(open("embedded\0nul", true), -1);
    }

    #[test]
    #[cfg(windows)]
    fn redirected_standard_streams_preserve_bytes_and_close() {
        use std::io::Write;
        const CHILD: &str = "LOOM_RUNTIME_STANDARD_STREAM_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let mut bytes = [0; 32];
            loop {
                let count = read(0, &mut bytes);
                assert!(count >= 0);
                if count == 0 {
                    break;
                }
                assert_eq!(write(1, &bytes[..count as usize]), count);
            }
            assert_eq!(write(2, "错误\n".as_bytes()), "错误\n".len() as i64);
            for fd in 0..3 {
                assert_eq!(close(fd), 0);
            }
            assert_eq!(close(1), -1);
            assert_eq!(write(1, b"closed"), -1);
            assert_eq!(read(0, &mut bytes), -1);
            std::process::exit(0);
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "file_io::tests::redirected_standard_streams_preserve_bytes_and_close",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let payload = "line\r\n\u{1a}\0界🙂".as_bytes();
        child.stdin.take().unwrap().write_all(payload).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.ends_with(payload));
        assert_eq!(output.stderr, "错误\n".as_bytes());
    }
}
