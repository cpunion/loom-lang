//! Real loopback smart-HTTP Git, using an ephemeral CA and synthetic credentials.
//! Only the trusted forwarding fixture receives this CA; host trust is unchanged.

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub struct Server {
    pub url: String,
    pub authenticated: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(root: &Path, git: &Path) -> Self {
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        ca.distinguished_name
            .push(rcgen::DnType::CommonName, "Loom Git fixture root");
        let key = KeyPair::generate().unwrap();
        fs::write(
            root.join("server-ca.pem"),
            ca.self_signed(&key).unwrap().pem(),
        )
        .unwrap();
        let issuer = Issuer::new(ca, key);
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec!["localhost".into()])
            .unwrap()
            .signed_by(&key, &issuer)
            .unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "https://localhost:{}/seed",
            listener.local_addr().unwrap().port()
        );
        fs::write(root.join("https-url"), &url).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let authenticated = Arc::new(AtomicUsize::new(0));
        let requests = authenticated.clone();
        let root = root.to_owned();
        let git = git.to_owned();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        socket.set_nonblocking(false).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_secs(20)))
                            .unwrap();
                        socket
                            .set_write_timeout(Some(Duration::from_secs(20)))
                            .unwrap();
                        serve(socket, config.clone(), &root, &git, &requests);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("Git HTTPS accept: {error}"),
                }
            }
        });
        Self {
            url,
            authenticated,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

fn serve(
    socket: TcpStream,
    config: Arc<rustls::ServerConfig>,
    root: &Path,
    git: &Path,
    requests: &AtomicUsize,
) {
    let mut stream =
        rustls::StreamOwned::new(rustls::ServerConnection::new(config).unwrap(), socket);
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        assert!(head.len() < 65536);
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).unwrap();
    let header = |name: &str| {
        head.lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
            .unwrap_or("")
    };
    assert!(
        header("Transfer-Encoding").is_empty(),
        "small fixture requests use Content-Length"
    );
    let mut body = vec![0; header("Content-Length").parse::<usize>().unwrap_or(0)];
    assert!(body.len() < 1_000_000);
    stream.read_exact(&mut body).unwrap();
    let (status, headers, body) =
        if header("Authorization") != super::AUTH_HEADER.strip_prefix("Authorization: ").unwrap() {
            (
                "401 Unauthorized".to_owned(),
                "WWW-Authenticate: Basic realm=\"fixture\"\r\n".to_owned(),
                Vec::new(),
            )
        } else {
            requests.fetch_add(1, Ordering::Relaxed);
            if root.join("auth-echo").exists() {
                (
                    "403 Forbidden".to_owned(),
                    "Content-Type: text/plain\r\n".to_owned(),
                    format!("{}\n{}", super::SENTINEL, super::AUTH_HEADER).into_bytes(),
                )
            } else {
                let mut request = head.lines().next().unwrap().split_whitespace();
                let method = request.next().unwrap();
                let target = request.next().unwrap();
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                let suffix = path.strip_prefix("/seed/").unwrap();
                assert!(matches!(suffix, "info/refs" | "git-upload-pack"));
                let mut command = Command::new(git);
                command.arg("http-backend").env_clear();
                for name in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP", "TMPDIR"] {
                    if let Some(value) = std::env::var_os(name) {
                        command.env(name, value);
                    }
                }
                let project_root = root.to_str().unwrap().replace('\\', "/");
                let project_root = project_root.strip_prefix("//?/").unwrap_or(&project_root);
                let mut child = command
                    .env("HOME", root.join("setup-home"))
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", root.join("setup-config"))
                    .env("GIT_PROJECT_ROOT", project_root)
                    .env("GIT_HTTP_EXPORT_ALL", "1")
                    .env("PATH_INFO", format!("/remote/.git/{suffix}"))
                    .env("REQUEST_METHOD", method)
                    .env("QUERY_STRING", query)
                    .env("CONTENT_TYPE", header("Content-Type"))
                    .env("CONTENT_LENGTH", body.len().to_string())
                    .env("HTTP_GIT_PROTOCOL", header("Git-Protocol"))
                    .env("SERVER_PROTOCOL", "HTTP/1.1")
                    .env("REMOTE_ADDR", "127.0.0.1")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                child.stdin.take().unwrap().write_all(&body).unwrap();
                let output = child.wait_with_output().unwrap();
                assert!(
                    output.status.success(),
                    "git http-backend: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let boundary = output
                    .stdout
                    .windows(4)
                    .position(|part| part == b"\r\n\r\n")
                    .unwrap();
                let mut status = "200 OK".to_owned();
                let mut headers = String::new();
                for line in std::str::from_utf8(&output.stdout[..boundary])
                    .unwrap()
                    .lines()
                {
                    if let Some(value) = line.strip_prefix("Status: ") {
                        status = value.to_owned();
                    } else {
                        headers.push_str(line);
                        headers.push_str("\r\n");
                    }
                }
                (status, headers, output.stdout[boundary + 4..].to_vec())
            }
        };
    write!(
        stream,
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    stream.conn.send_close_notify();
    stream.flush().unwrap();
}
