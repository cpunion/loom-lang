use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    sync::Arc,
    thread,
    time::Duration,
};

mod common;
use common::success;

fn serve(stream: impl Read + Write, mode: &str, payload: &[u8], location: &str) {
    let mut stream = stream;
    if mode == "wrong-name" {
        let mut byte = [0];
        assert!(!matches!(stream.read(&mut byte), Ok(1)));
        return;
    }
    let mut metadata = Vec::new();
    while !metadata.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        metadata.push(byte[0]);
        assert!(metadata.len() < 65536);
    }
    let text = String::from_utf8(metadata).unwrap();
    assert!(text.starts_with(if mode == "ok" {
        "POST /echo?x=1 HTTP/1.1\r\n"
    } else {
        "GET /echo?x=1 HTTP/1.1\r\n"
    }));
    assert!(text.contains("\r\nConnection: close\r\n"));
    if mode != "stream" {
        assert!(text.contains("\r\nX-Snapshot: before\r\n"));
    }
    let size: usize = text
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .unwrap()
        .parse()
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    if mode == "ok" || mode == "stream" {
        if mode == "ok" {
            assert_eq!(body, payload);
        } else {
            assert!(body.is_empty());
        }
        stream
            .write_all(
                format!(
                    "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                )
                .as_bytes(),
            )
            .unwrap();
        stream.write_all(payload).unwrap();
    } else if mode == "redirect" {
        stream
            .write_all(
                format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n")
                    .as_bytes(),
            )
            .unwrap();
    } else if mode == "interim-limit" {
        stream
            .write_all(b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 103 Early Hints\r\n\r\n")
            .unwrap();
    } else {
        assert_eq!(mode, "limit");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
            .unwrap();
    }
    stream.flush().unwrap();
    let mut byte = [0];
    assert!(!matches!(stream.read(&mut byte), Ok(1)));
}

#[test]
fn source_https_client_verifies_names_limits_and_never_follows_redirects() {
    let directory = tempfile::tempdir().unwrap();
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let key = KeyPair::generate().unwrap();
    fs::write(
        directory.path().join("ca.pem"),
        ca.self_signed(&key).unwrap().pem(),
    )
    .unwrap();
    let issuer = Issuer::new(ca, key);
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .signed_by(&leaf_key, &issuer)
        .unwrap();
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![leaf.der().clone()],
        leaf_key.serialize_der().try_into().unwrap(),
    )
    .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let config = Arc::new(config);
    let payload: Vec<u8> = (0..131079).map(|index| (index % 256) as u8).collect();
    fs::write(directory.path().join("payload"), &payload).unwrap();
    let executable = common::executable(directory.path(), "http-client");
    let streaming = common::executable(directory.path(), "http-streaming-client");
    let tests = common::executable(directory.path(), "http-client-tests");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "test",
                "compiler/std/net/http/client",
                "--no-run",
                "--output",
                tests.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&common::run_task_command(&mut Command::new(&tests)));
        success(&common::run_tasks(&tests));
        success(
            &common::command(&[
                "build",
                "compiler/examples/http_client",
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(
            &common::command(&[
                "build",
                "compiler/examples/http_streaming_client",
                "--output",
                streaming.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        for mode in [
            "ok",
            "wrong-name",
            "limit",
            "interim-limit",
            "redirect",
            "stream",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let cloned = listener.try_clone().unwrap();
            let config = config.clone();
            let payload = payload.clone();
            let peer = thread::spawn(move || {
                let (socket, _) = cloned.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(config).unwrap(),
                    socket,
                );
                serve(
                    stream,
                    mode,
                    &payload,
                    &format!("https://localhost:{port}/follow"),
                );
            });
            let host = if mode == "wrong-name" {
                "127.0.0.1"
            } else {
                "localhost"
            };
            let mut command = Command::new(if mode == "stream" {
                &streaming
            } else {
                &executable
            });
            command
                .arg(format!("https://{host}:{port}/echo?x=1#local"))
                .arg(directory.path().join("ca.pem"))
                .env("LOOM_GC_STRESS", "1");
            if mode != "stream" {
                command.arg(directory.path().join("payload")).arg(mode);
            }
            let output = common::run_task_command(&mut command);
            peer.join().unwrap();
            success(&output);
            assert_eq!(
                output.stdout,
                match mode {
                    "ok" => b"HTTPS response\n".as_slice(),
                    "wrong-name" => b"peer rejected\n".as_slice(),
                    "redirect" => b"redirect returned\n".as_slice(),
                    "stream" => b"200 131079\n".as_slice(),
                    _ => b"bounded protocol rejection\n".as_slice(),
                }
            );
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
}
