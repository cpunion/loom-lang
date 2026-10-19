# Streaming a file into HTTP/1.1

```sh
target/loom check compiler/examples/http_upload
target/loom build compiler/examples/http_upload --output target/http-upload
target/http-upload 127.0.0.1:8080 ./document.bin
```

Use a local HTTP/1.1 test server accepting a chunked POST at `/upload` and
replying with status 200 and body `accepted`. The native integration test
provides an independent Rust peer on an ephemeral port; no external service is
needed for CI.

The application borrows a scoped file producer with 8 KiB chunks into
`std.net.http.write_request`. It sends binary contents and a separate X-End
trailer without collecting the file. The TCP connection closes through lexical
defer after its directly awaited operations; early failure also releases the
file. Unit tests cover backpressure, quotas, typed failures and cancellation.

This is a low-level message-codec example over explicit plaintext TCP, not the
HTTPS client's URL/trust/security policy. It waits for upload completion before
reading the response; duplex early-response handling and deadlines are caller
policy. Do not use this plaintext example to send credentials or private data.
