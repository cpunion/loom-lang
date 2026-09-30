use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use std::{fs, path::Path, process::Command};
mod common;
use common::success;

const PACKAGE: &str = "compiler/examples/tls_loopback";

fn certificates(directory: &Path) {
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "Loom server fixture root");
    let key = KeyPair::generate().unwrap();
    fs::write(
        directory.join("ca.pem"),
        ca.self_signed(&key).unwrap().pem(),
    )
    .unwrap();
    let issuer = Issuer::new(ca, key);
    let leaf_key = KeyPair::generate().unwrap();
    let mut leaf = CertificateParams::new(vec!["localhost".into()]).unwrap();
    fs::write(
        directory.join("cert.pem"),
        leaf.signed_by(&leaf_key, &issuer).unwrap().pem(),
    )
    .unwrap();
    leaf.not_before = rcgen::date_time_ymd(2000, 1, 1);
    leaf.not_after = rcgen::date_time_ymd(2001, 1, 1);
    fs::write(
        directory.join("expired.pem"),
        leaf.signed_by(&leaf_key, &issuer).unwrap().pem(),
    )
    .unwrap();
    fs::write(directory.join("key.pem"), leaf_key.serialize_pem()).unwrap();
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "Loom client fixture root");
    let key = KeyPair::generate().unwrap();
    fs::write(
        directory.join("client-ca.pem"),
        ca.self_signed(&key).unwrap().pem(),
    )
    .unwrap();
    let issuer = Issuer::new(ca, key);
    let key = KeyPair::generate().unwrap();
    let mut leaf = CertificateParams::new(Vec::<String>::new()).unwrap();
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    fs::write(
        directory.join("client.pem"),
        leaf.signed_by(&key, &issuer).unwrap().pem(),
    )
    .unwrap();
    fs::write(directory.join("client-key.pem"), key.serialize_pem()).unwrap();
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    fs::write(
        directory.join("client-purpose.pem"),
        leaf.signed_by(&key, &issuer).unwrap().pem(),
    )
    .unwrap();
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    leaf.not_before = rcgen::date_time_ymd(2000, 1, 1);
    leaf.not_after = rcgen::date_time_ymd(2001, 1, 1);
    fs::write(
        directory.join("client-expired.pem"),
        leaf.signed_by(&key, &issuer).unwrap().pem(),
    )
    .unwrap();
    fs::write(
        directory.join("payload"),
        (0..131_079).map(|i| (i % 256) as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    fs::write(
        directory.join("large-payload"),
        (0..8 * 1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
}

#[test]
fn tls_roundtrip_verifies_peers_and_survives_moving_gc() {
    let directory = tempfile::tempdir().unwrap();
    certificates(directory.path());
    let executable = common::executable(directory.path(), "tls");
    success(&common::loom(&["check", PACKAGE]));
    success(&common::loom(&["test", "compiler/std/net/tls"]));
    success(&common::run_tasks(&common::executable(
        &common::root().join("compiler/std/net/tls/target"),
        "tests",
    )));
    for level in ["0", "2"] {
        success(
            &common::command(&["build", PACKAGE, "--output", executable.to_str().unwrap()])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        for mode in ["ok", "duplex", "wrong-name", "untrusted", "expired"] {
            let output = common::run_task_command(
                Command::new(&executable)
                    .current_dir(directory.path())
                    .env("LOOM_GC_STRESS", "1")
                    .args([
                        "ca.pem",
                        if mode == "expired" {
                            "expired.pem"
                        } else {
                            "cert.pem"
                        },
                        "key.pem",
                        "payload",
                        mode,
                    ]),
            );
            success(&output);
            assert_eq!(
                output.stdout,
                if mode == "ok" || mode == "duplex" {
                    b"TLS roundtrip\n".as_slice()
                } else {
                    b"peer rejected\n".as_slice()
                }
            );
        }
        for (mode, roots, certificate) in [
            ("mutual", "client-ca.pem", "client.pem"),
            ("missing-client", "client-ca.pem", "client.pem"),
            ("untrusted-client", "ca.pem", "client.pem"),
            ("expired-client", "client-ca.pem", "client-expired.pem"),
            ("purpose-client", "client-ca.pem", "client-purpose.pem"),
        ] {
            let output = common::run_task_command(
                Command::new(&executable)
                    .current_dir(directory.path())
                    .env("LOOM_GC_STRESS", "1")
                    .args([
                        "ca.pem",
                        "cert.pem",
                        "key.pem",
                        "payload",
                        mode,
                        roots,
                        certificate,
                        "client-key.pem",
                    ]),
            );
            success(&output);
            assert_eq!(
                output.stdout,
                if mode == "mutual" {
                    b"TLS roundtrip\n".as_slice()
                } else {
                    b"peer rejected\n".as_slice()
                }
            );
        }
        // Both peers send beyond their initial socket windows before their
        // writers finish. Reads must not wait behind those pending writes.
        success(&common::run_task_command(
            Command::new(&executable)
                .current_dir(directory.path())
                .args(["ca.pem", "cert.pem", "key.pem", "large-payload", "duplex"]),
        ));
    }
    let cache = directory.path().join("cache");
    for expected in ["miss", "hit"] {
        let output = common::command(&["build", PACKAGE, "--output", executable.to_str().unwrap()])
            .arg("--object-cache")
            .arg(&cache)
            .env("LOOM_NATIVE_TIMINGS", "1")
            .output()
            .unwrap();
        success(&output);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(&format!("loom cache: {expected}"))
        );
    }
    success(&common::run_task_command(
        Command::new(&executable)
            .current_dir(directory.path())
            .env("LOOM_GC_STRESS", "1")
            .args(["ca.pem", "cert.pem", "key.pem", "payload", "ok"]),
    ));
    let output = common::command(&["run", PACKAGE])
        .arg("--")
        .args(["ca.pem", "cert.pem", "key.pem", "payload"].map(|name| directory.path().join(name)))
        .arg("ok")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"TLS roundtrip\n");
}

#[test]
fn unused_tls_imports_need_only_the_core_archive() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    fs::create_dir(&package).unwrap();
    fs::write(
        package.join("main.loom"),
        r#"
import std.net.tls.default_client
import std.time.sleep_ns

async fn main() {
    sleep_ns(1).await
}
"#,
    )
    .unwrap();
    let name = if cfg!(windows) {
        "loom_runtime.lib"
    } else {
        "libloom_runtime.a"
    };
    let archive = directory.path().join(name);
    let original = std::env::var_os("LOOM_RUNTIME_LIBRARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_BIN_EXE_loom-native")).with_file_name(name));
    fs::copy(original, &archive).unwrap();
    let executable = common::executable(directory.path(), "core-only");
    success(
        &common::command(&[
            "build",
            package.to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
        ])
        .env("LOOM_RUNTIME_LIBRARY", &archive)
        .output()
        .unwrap(),
    );
    success(&common::run_tasks(&executable));
    let bytes = fs::read(executable).unwrap();
    for symbol in [b"loom_rt_tls_".as_slice(), b"ring_core_"] {
        assert!(!bytes.windows(symbol.len()).any(|window| window == symbol));
    }
    // An actual reference must fail, not fall back to the incomplete core archive.
    let output = common::command(&[
        "build",
        PACKAGE,
        "--output",
        directory.path().join("missing").to_str().unwrap(),
    ])
    .env("LOOM_RUNTIME_LIBRARY", &archive)
    .output()
    .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing native runtime"));
}

#[test]
fn native_peer_distinguishes_tls_eof_truncation_and_cancelled_reads() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::Arc,
        thread,
        time::Duration,
    };
    let directory = tempfile::tempdir().unwrap();
    certificates(directory.path());
    let chain = fs::read(directory.path().join("cert.pem")).unwrap();
    let key = fs::read(directory.path().join("key.pem")).unwrap();
    let client_ca = fs::read(directory.path().join("client-ca.pem")).unwrap();
    let client_leaf = fs::read(directory.path().join("client.pem")).unwrap();
    let client_leaf = rustls_pemfile::certs(&mut client_leaf.as_slice())
        .next()
        .unwrap()
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_pemfile::certs(&mut client_ca.as_slice()) {
        roots.add(certificate.unwrap()).unwrap();
    }
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .unwrap();
    let executable = common::executable(directory.path(), "peer");
    // The loopback test covers both optimization levels with the default TLS
    // negotiation; these interoperability profiles also exercise TLS 1.2.
    for (level, version) in [
        ("0", &rustls::version::TLS12),
        ("2", &rustls::version::TLS13),
    ] {
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[version])
        .unwrap()
        .with_client_cert_verifier(verifier.clone())
        .with_single_cert(
            rustls_pemfile::certs(&mut chain.as_slice())
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            rustls_pemfile::private_key(&mut key.as_slice())
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        config.alpn_protocols = vec![b"loom-echo".to_vec()];
        let config = Arc::new(config);
        success(
            &common::command(&[
                "build",
                "compiler/tests/fixtures/tls_peer",
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        for mode in [
            "graceful",
            "truncated",
            "busy",
            "cancel-read",
            "cancel-write",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let config = config.clone();
            let client_leaf = client_leaf.clone();
            let (completed, client_done) = std::sync::mpsc::channel();
            let peer = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                if mode.starts_with("cancel-") {
                    socket2::SockRef::from(&socket)
                        .set_recv_buffer_size(4096)
                        .unwrap();
                }
                socket
                    .set_read_timeout(Some(Duration::from_secs(20)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(20)))
                    .unwrap();
                let mut connection = rustls::ServerConnection::new(config).unwrap();
                let mut stream = rustls::Stream::new(&mut connection, &mut socket);
                let mut request = [0; 4];
                stream.read_exact(&mut request).unwrap();
                assert_eq!(&request, b"ping");
                // A completed client write need not have reached this reader.
                // Acknowledge it before the busy test cancels/closes the socket.
                stream.write_all(b"pong").unwrap();
                stream.flush().unwrap();
                if mode.starts_with("cancel-") {
                    // Keep the peer's receive window closed until the client
                    // cancels and drains both directions. No timing-based read
                    // race can make the pending client write complete early.
                    client_done.recv_timeout(Duration::from_secs(20)).unwrap();
                } else if mode == "busy" {
                    assert!(stream.read(&mut request).is_err());
                } else if mode == "graceful" {
                    connection.send_close_notify();
                    while connection.wants_write() {
                        connection.write_tls(&mut socket).unwrap();
                    }
                }
                assert_eq!(connection.peer_certificates().unwrap()[0], client_leaf);
            });
            let output = common::run_task_command(
                Command::new(&executable)
                    .env(
                        "LOOM_GC_STRESS",
                        if mode.starts_with("cancel-") {
                            "0"
                        } else {
                            "1"
                        },
                    )
                    .arg(directory.path().join("ca.pem"))
                    .arg(address.to_string())
                    .arg(mode)
                    .arg(directory.path().join("client.pem"))
                    .arg(directory.path().join("client-key.pem"))
                    .arg(directory.path().join("large-payload")),
            );
            let _ = completed.send(());
            success(&output);
            peer.join().unwrap();
        }
    }
}
