# Verified TLS loopback

The client and server exchange binary bytes over a real loopback TCP socket,
negotiate TLS 1.3 and `loom-echo` with ALPN and use close-notify to delimit a request
without closing its response channel. All scheduling and cleanup use source
Tasks; the separately linked Rustls engine only processes TLS packets.

```sh
target/loom check compiler/examples/tls_loopback
target/loom build compiler/examples/tls_loopback
target/loom test compiler/std/net/tls
cargo test -p loom-native --test tls_transport
```

The integration test creates temporary CA/leaf certificates and a 128 KiB binary
payload, then runs O0/O2 under moving-GC stress. It also rejects wrong names,
unknown roots and expired certificates, verifies mutual TLS with a separate
client CA (including absent, untrusted, expired and wrong-purpose client
certificates), checks cached linking and unused-import
DCE, and interoperates with a Rustls peer for clean EOF, truncation and cancellation
of either direction while both are pending. Concurrent transfers exchange 8 MiB
in each direction; the smaller transfer also runs under moving-GC stress. The
independent peer holds a small receive window until the client finishes, forcing
backpressure during cancellation. Its profiles exercise TLS 1.2 and 1.3; TLS 1.2 peers can
close both directions on close-notify, so applications must not assume that a
response can follow their TLS 1.2 write shutdown. No external server, persistent
private key or verifier bypass is needed.

To run manually, provide a CA PEM, a matching server chain/key for `localhost`,
and a payload file larger than 64 KiB:

```sh
target/loom run compiler/examples/tls_loopback -- ca.pem cert.pem key.pem payload.bin ok
```

Use `duplex` instead of `ok` to send and receive simultaneously on both peers.

For mutual TLS, append the client CA, client certificate chain and matching key:

```sh
target/loom run compiler/examples/tls_loopback -- ca.pem cert.pem key.pem payload.bin mutual client-ca.pem client.pem client-key.pem
```

`ServerOptions` requires an `Identity` and an explicit `ClientAuth` policy.
`ClientAuth.Required(bytes)` requires a client chaining to those PEM roots;
`ClientAuth.Anonymous` does not request a client certificate. `ClientOptions`
supplies `Option[Identity]` separately from server trust. `peer_certificate`
copies the authenticated leaf DER; interpreting its identity and deciding
application permissions remain library/application policy.

Do not commit private keys. `Trust.Builtin` uses compiled Mozilla roots, not the
OS trust store; `Trust.Certificates(bytes)` explicitly supplies custom roots.
One reader and one writer may run together across connection aliases; overlapping
reads or overlapping writes return `Busy`. Cancellation drains that operation,
retires the connection and wakes the other direction to fail. Revocation and
resumption configuration are not exposed.
