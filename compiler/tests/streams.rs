use std::process::Command;

mod common;
use common::success;

#[test]
fn streamed_http_file_uploads_interoperate_without_buffering_the_document() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("payload");
    let payload: Vec<u8> = (0..65539).map(|index| (index % 256) as u8).collect();
    std::fs::write(&path, &payload).unwrap();
    let executable = common::executable(temporary.path(), "http-upload");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", "compiler/examples/http_upload"])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected = payload.clone();
        let peer = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut stream = BufReader::new(socket);
            let mut line = String::new();
            let mut fields = Vec::new();
            loop {
                line.clear();
                assert!(stream.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                fields.push(line.clone());
            }
            assert_eq!(fields[0], "POST /upload HTTP/1.1\r\n");
            assert!(fields.contains(&format!("Host: {address}\r\n")));
            assert!(fields.contains(&"Transfer-Encoding: chunked\r\n".into()));
            assert!(
                !fields
                    .iter()
                    .any(|line| line.starts_with("Content-Length:"))
            );
            let mut body = Vec::new();
            let mut chunks = 0;
            loop {
                line.clear();
                stream.read_line(&mut line).unwrap();
                let size = usize::from_str_radix(line.strip_suffix("\r\n").unwrap(), 16).unwrap();
                if size == 0 {
                    break;
                }
                assert!(size <= 8192);
                let start = body.len();
                body.resize(start + size, 0);
                stream.read_exact(&mut body[start..]).unwrap();
                let mut crlf = [0; 2];
                stream.read_exact(&mut crlf).unwrap();
                assert_eq!(crlf, *b"\r\n");
                chunks += 1;
            }
            assert!(chunks > 1);
            assert_eq!(body, expected);
            line.clear();
            stream.read_line(&mut line).unwrap();
            assert_eq!(line, "X-End: complete\r\n");
            line.clear();
            stream.read_line(&mut line).unwrap();
            assert_eq!(line, "\r\n");
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\naccepted")
                .unwrap();
            let mut byte = [0];
            assert!(!matches!(stream.read(&mut byte), Ok(1)));
        });
        let output = common::run_task_command(
            Command::new(&executable)
                .arg(address.to_string())
                .arg(&path)
                .env("LOOM_GC_STRESS", "1"),
        );
        peer.join().unwrap();
        success(&output);
        assert_eq!(output.stdout, b"streamed upload accepted\n");
    }
}

#[test]
fn source_framing_keeps_binary_text_boundaries_limits_and_real_tcp_cleanup() {
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "framed-stream");
    for package in [
        "compiler/std/stream/length32",
        "compiler/examples/framed_tcp",
        "compiler/std/stream/lines",
        "compiler/examples/tcp_lines",
        "compiler/std/net/http",
        "compiler/std/net/http/url",
        "compiler/examples/http_loopback",
    ] {
        success(&common::loom(&["check", package]));
        for level in ["0", "2"] {
            success(
                &common::command(&["test", package, "--no-run"])
                    .arg("--output")
                    .arg(&executable)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            success(&common::run_tasks(&executable));
        }
    }
    for package in [
        "compiler/examples/framed_tcp",
        "compiler/examples/tcp_lines",
        "compiler/examples/http_loopback",
    ] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", "2")
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&executable));
    }
}

#[test]
fn source_streams_keep_typed_pulls_real_io_and_task_cleanup() {
    let temporary = tempfile::tempdir().unwrap();
    for package in ["compiler/std/stream", "compiler/std/net/tcp/chunks"] {
        success(&common::loom(&["check", package]));
        let executable = common::executable(temporary.path(), "stream-tests");
        for level in ["0", "2"] {
            success(
                &common::command(&["test", package, "--no-run"])
                    .arg("--output")
                    .arg(&executable)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            success(&common::run_tasks(&executable));
        }
    }
    let example = "compiler/examples/streams";
    for mode in ["check", "test", "run"] {
        success(&common::loom(&[mode, example]));
    }
    let executable = common::executable(temporary.path(), "stream-example");
    success(
        &common::command(&["build", example])
            .arg("--output")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = Command::new(executable)
        .env("LOOM_GC_STRESS", "1")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"item 0 ready\nitem 1 ready\nitem 2 ready\n");
}

#[test]
fn scoped_async_file_pipelines_keep_errors_and_stop_before_the_suffix() {
    let package = "compiler/examples/stream_lines";
    for mode in ["check", "test"] {
        success(&common::loom(&[mode, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "stream-lines");
    let input = temporary.path().join("input.txt");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        for (contents, expected) in [
            (
                &b"\nfirst\r\n\nsecond\n\xff\n"[..],
                Some(&b"first\nsecond\n"[..]),
            ),
            (&b"\nfirst\n\xff\n"[..], None),
        ] {
            std::fs::write(&input, contents).unwrap();
            let output = common::run_task_command(
                Command::new(&executable)
                    .arg(&input)
                    .env("LOOM_GC_STRESS", "1"),
            );
            match expected {
                Some(text) => {
                    success(&output);
                    assert_eq!(output.stdout, text);
                }
                None => {
                    assert_eq!(output.status.code(), Some(1));
                    assert_eq!(output.stderr, b"cannot read UTF-8 lines\n");
                }
            }
        }
    }
    std::fs::write(&input, "first\nsecond\n").unwrap();
    let output = common::command(&["run", package])
        .arg("--")
        .arg(&input)
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"first\nsecond\n");
}

#[test]
fn binary_file_streams_bound_chunks_and_keep_scoped_worker_cleanup() {
    let library = "compiler/std/file/chunks";
    let example = "compiler/examples/file_chunks";
    for package in [library, example, "compiler/examples/file_chunks/wire"] {
        success(&common::loom(&["check", package]));
        success(&common::loom(&["test", package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let tests = common::executable(temporary.path(), "file-chunk-tests");
    let executable = common::executable(temporary.path(), "file-chunks");
    let input = temporary.path().join("input.bin");
    let missing = temporary.path().join("absent.bin");
    for level in ["0", "2"] {
        for package in [library, example] {
            success(
                &common::command(&["test", package, "--no-run"])
                    .arg("--output")
                    .arg(&tests)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            success(&common::run_task_command(
                Command::new(&tests)
                    .current_dir(temporary.path())
                    .env("LOOM_GC_STRESS", "1"),
            ));
        }
        success(
            &common::command(&["build", example])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        for contents in [vec![], vec![0, 255, 128, 10, 13], vec![255; 9001]] {
            std::fs::write(&input, &contents).unwrap();
            let output = common::run_task_command(
                Command::new(&executable)
                    .arg(&input)
                    .env("LOOM_GC_STRESS", "1"),
            );
            success(&output);
            assert_eq!(output.stdout, format!("{}\n", contents.len()).as_bytes());
        }
        let output = common::run_task_command(Command::new(&executable).arg(&missing));
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stderr, b"cannot read file\n");
    }
    let output = common::command(&["run", example])
        .arg("--")
        .arg(&input)
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"9001\n");
}
