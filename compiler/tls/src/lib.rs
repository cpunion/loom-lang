//! TLS packet engines, not an I/O executor. Loom owns transport, suspension,
//! operation exclusion, cancellation and close policy. Sessions retain only
//! Rust-owned protocol state; managed input is copied before returning.

use loom_runtime::native::{self, bytes as buffer_bytes, text as text_bytes};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{
    ClientConfig, ClientConnection, Connection, RootCertStore, ServerConfig, ServerConnection,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::{Arc, OnceLock};

const PROTOCOL: i64 = -1;
const PENDING: i64 = -2;
const PEER: i64 = -3;
const CLOSED: i64 = -4;
const BUFFER_LIMIT: usize = 64 * 1024;

fn certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, i64> {
    let values = rustls_pemfile::certs(&mut &pem[..])
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| PROTOCOL)?;
    if values.is_empty() {
        Err(PROTOCOL)
    } else {
        Ok(values)
    }
}

fn roots(pem: &[u8], builtin: bool) -> Result<Arc<RootCertStore>, i64> {
    if builtin {
        static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
        return Ok(ROOTS
            .get_or_init(|| {
                Arc::new(RootCertStore {
                    roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
                })
            })
            .clone());
    }
    let mut roots = RootCertStore::empty();
    for certificate in certificates(pem)? {
        roots.add(certificate).map_err(|_| PROTOCOL)?;
    }
    Ok(Arc::new(roots))
}

fn client(
    name: &str,
    pem: &[u8],
    builtin: bool,
    protocols: Vec<Vec<u8>>,
) -> Result<Connection, i64> {
    let name = ServerName::try_from(name.to_owned()).map_err(|_| PROTOCOL)?;
    let mut config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| PROTOCOL)?
            .with_root_certificates(roots(pem, builtin)?)
            .with_no_client_auth();
    config.alpn_protocols = protocols;
    // Default verifier, time validation, no early data, no key log and no
    // dangerous bypass. Certificate/hostname failures remain terminal errors.
    ClientConnection::new(Arc::new(config), name)
        .map(Connection::Client)
        .map_err(|_| PROTOCOL)
}

fn server(chain: &[u8], key: &[u8], protocols: Vec<Vec<u8>>) -> Result<Connection, i64> {
    let key = rustls_pemfile::private_key(&mut &key[..])
        .map_err(|_| PROTOCOL)?
        .ok_or(PROTOCOL)?;
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| PROTOCOL)?
            .with_no_client_auth()
            .with_single_cert(certificates(chain)?, key)
            .map_err(|_| PROTOCOL)?;
    config.alpn_protocols = protocols;
    ServerConnection::new(Arc::new(config))
        .map(Connection::Server)
        .map_err(|_| PROTOCOL)
}

struct Session {
    connection: Connection,
    write_closed: bool,
}

impl Session {
    fn receive(&mut self, input: &[u8], eof: bool) -> Result<(), i64> {
        let mut input = input;
        loop {
            let count = self.connection.read_tls(&mut input).map_err(|_| PROTOCOL)?;
            self.connection.process_new_packets().map_err(|error| {
                if matches!(error, rustls::Error::InvalidCertificate(_)) {
                    PEER
                } else {
                    PROTOCOL
                }
            })?;
            if input.is_empty() {
                break;
            }
            if count == 0 {
                return Err(PROTOCOL);
            }
        }
        if eof && self.connection.is_handshaking() {
            return Err(PROTOCOL);
        }
        Ok(())
    }

    fn output(&mut self) -> Result<Vec<u8>, i64> {
        let mut output = Vec::new();
        self.connection
            .write_tls(&mut output)
            .map_err(|_| PROTOCOL)?;
        Ok(output)
    }

    fn read(&mut self, limit: usize) -> Result<Vec<u8>, i64> {
        let mut output = vec![0; limit.min(BUFFER_LIMIT)];
        match self.connection.reader().read(&mut output) {
            Ok(count) => {
                output.truncate(count);
                Ok(output)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(PENDING),
            Err(_) => Err(PROTOCOL),
        }
    }

    fn write(&mut self, input: &[u8]) -> i64 {
        if self.write_closed {
            return CLOSED;
        }
        self.connection
            .writer()
            .write(input)
            .map_or(PROTOCOL, |count| count as i64)
    }
}

#[derive(Default)]
struct Sessions {
    entries: RefCell<HashMap<i64, Session>>,
    next: Cell<i64>,
}

impl Sessions {
    fn insert(&self, mut connection: Connection) -> i64 {
        connection.set_buffer_limit(Some(BUFFER_LIMIT));
        let token = self
            .next
            .get()
            .checked_add(1)
            .expect("TLS identities exhausted");
        self.entries.borrow_mut().insert(
            token,
            Session {
                connection,
                write_closed: false,
            },
        );
        self.next.set(token);
        token
    }

    fn with<T>(&self, token: i64, apply: impl FnOnce(&mut Session) -> T) -> Option<T> {
        self.entries.borrow_mut().get_mut(&token).map(apply)
    }
}

unsafe fn protocols(pointer: *const u8) -> Result<Vec<Vec<u8>>, i64> {
    // SAFETY: Checked private ABI requires List[Text]. No managed allocation
    // occurs while copying the initialized Text elements into native storage.
    let result = unsafe { native::texts(pointer) };
    for bytes in &result {
        if bytes.is_empty() || bytes.len() > 255 {
            return Err(PROTOCOL);
        }
    }
    Ok(result)
}

#[unsafe(no_mangle)]
unsafe extern "C-unwind" fn loom_rt_tls_client(
    name: *const u8,
    roots: *const u8,
    builtin: i64,
    alpn: *const u8,
) -> i64 {
    if builtin != 0 && builtin != 1 {
        return PROTOCOL;
    }
    // SAFETY: Inputs are live checked Text/Bytes/List[Text]. Configuration and
    // protocol state own their bytes before this non-GC-allocating call returns.
    let connection = unsafe {
        protocols(alpn).and_then(|alpn| {
            client(
                std::str::from_utf8_unchecked(text_bytes(name)),
                buffer_bytes(roots),
                builtin == 1,
                alpn,
            )
        })
    };
    connection.map_or_else(
        |error| error,
        |connection| native::context::<Sessions, _>(|sessions| sessions.insert(connection)),
    )
}

#[unsafe(no_mangle)]
unsafe extern "C-unwind" fn loom_rt_tls_server(
    chain: *const u8,
    key: *const u8,
    alpn: *const u8,
) -> i64 {
    // SAFETY: Live checked Bytes/List[Text]; the resulting engine retains no
    // managed pointers. No keys, certificates or remote errors are formatted.
    let connection = unsafe {
        protocols(alpn).and_then(|alpn| server(buffer_bytes(chain), buffer_bytes(key), alpn))
    };
    connection.map_or_else(
        |error| error,
        |connection| native::context::<Sessions, _>(|sessions| sessions.insert(connection)),
    )
}

#[unsafe(no_mangle)]
unsafe extern "C-unwind" fn loom_rt_tls_receive(token: i64, input: *const u8, eof: i64) -> i64 {
    // SAFETY: The Bytes input stays rooted and no Loom allocation occurs here.
    let input = unsafe { buffer_bytes(input) };
    if (eof != 0 && eof != 1) || (eof == 1 && !input.is_empty()) {
        return PROTOCOL;
    }
    if eof == 0 && input.is_empty() {
        return 0;
    }
    native::context::<Sessions, _>(|sessions| {
        sessions
            .with(token, |session| {
                session
                    .receive(input, eof == 1)
                    .map_or_else(|error| error, |()| 0)
            })
            .unwrap_or(CLOSED)
    })
}

unsafe fn append_output(pointer: *mut u8, output: Result<Vec<u8>, i64>) -> i64 {
    let bytes = match output {
        Ok(bytes) => bytes,
        Err(error) => return error,
    };
    // SAFETY: Rooted managed output, independent native input and no table borrow.
    unsafe { native::append(pointer, &bytes) }
}

#[unsafe(no_mangle)]
unsafe extern "C-unwind" fn loom_rt_tls_output(token: i64, buffer: *mut u8) -> i64 {
    let output = native::context::<Sessions, _>(|sessions| {
        sessions.with(token, Session::output).unwrap_or(Err(CLOSED))
    });
    // SAFETY: The checked caller supplies a rooted Bytes output.
    unsafe { append_output(buffer, output) }
}

#[unsafe(no_mangle)]
unsafe extern "C-unwind" fn loom_rt_tls_read(token: i64, buffer: *mut u8, limit: i64) -> i64 {
    let Ok(limit) = usize::try_from(limit) else {
        return PROTOCOL;
    };
    if limit == 0 {
        return PROTOCOL;
    }
    let output = native::context::<Sessions, _>(|sessions| {
        sessions
            .with(token, |session| session.read(limit))
            .unwrap_or(Err(CLOSED))
    });
    // SAFETY: The checked caller supplies a rooted Bytes output.
    unsafe { append_output(buffer, output) }
}

#[unsafe(no_mangle)]
unsafe extern "C-unwind" fn loom_rt_tls_write(
    token: i64,
    input: *const u8,
    offset: i64,
    end: i64,
) -> i64 {
    let (Ok(offset), Ok(end)) = (usize::try_from(offset), usize::try_from(end)) else {
        return PROTOCOL;
    };
    // SAFETY: Rooted Bytes, no Loom allocation. Native protocol buffers copy
    // this range, and later alias appends stay outside the operation's end.
    let Some(input) = (unsafe { buffer_bytes(input) }).get(offset..end) else {
        return PROTOCOL;
    };
    native::context::<Sessions, _>(|sessions| {
        sessions
            .with(token, |session| session.write(input))
            .unwrap_or(CLOSED)
    })
}

#[unsafe(no_mangle)]
extern "C-unwind" fn loom_rt_tls_status(token: i64) -> i64 {
    native::context::<Sessions, _>(|sessions| {
        sessions
            .with(token, |session| {
                i64::from(session.connection.is_handshaking())
            })
            .unwrap_or(CLOSED)
    })
}

#[unsafe(no_mangle)]
extern "C-unwind" fn loom_rt_tls_shutdown(token: i64) -> i64 {
    native::context::<Sessions, _>(|sessions| {
        sessions
            .with(token, |session| {
                session.connection.send_close_notify();
                session.write_closed = true;
                0
            })
            .unwrap_or(CLOSED)
    })
}

#[unsafe(no_mangle)]
extern "C-unwind" fn loom_rt_tls_release(token: i64) -> i64 {
    native::context::<Sessions, _>(|sessions| {
        if sessions.entries.borrow_mut().remove(&token).is_some() {
            0
        } else {
            CLOSED
        }
    })
}

#[unsafe(no_mangle)]
extern "C-unwind" fn loom_rt_tls_protocol(token: i64) -> *mut u8 {
    let bytes = native::context::<Sessions, _>(|sessions| {
        sessions
            .with(token, |session| {
                session
                    .connection
                    .alpn_protocol()
                    .unwrap_or_default()
                    .to_vec()
            })
            .unwrap_or_default()
    });
    // SAFETY: ALPN can select only a supplied UTF-8 Text protocol; input is
    // independent native bytes and no session borrow survives allocation.
    unsafe { native::copy_text(&bytes) }
}
