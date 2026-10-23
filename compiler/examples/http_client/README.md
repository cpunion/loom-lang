# HTTP client interoperability fixture

This compiled Loom client uses `std.net.http.client` against an independent
Rustls HTTP/1.1 server. It snapshots an uploaded binary body, verifies an explicit
CA and hostname, bounds final/informational responses and distinguishes default
redirect responses from explicit bounded same-origin following.

Run the complete local fixture (temporary CA/keys/server, no external service):

```sh
cargo test -p loom-native --test http_client
target/loom test compiler/std/net/http/client
```

The native gate covers O0/O2 and moving-GC stress, including detached TLS/header
snapshots across HTTPS hops, cross-origin refusal and redirect limits.
The source tests also force
both socket windows below an early final response/upload, and observe peer close
after a real established request is cancelled or times out.

For an already running echo fixture, the executable accepts `URL CA_PEM PAYLOAD
MODE`; `ok` expects an echoed nonempty POST body. Other fixture modes are
`wrong-name`, `limit`, `interim-limit`, `redirect`, `follow`, `follow-origin` and
`follow-limit`. It prints only result labels,
not URLs, certificates, headers or response bodies. Do not commit private keys.

For application code and supported policies, see the
[client API](../../std/net/http/client/README.md).
