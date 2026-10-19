# Stream a file over verified HTTPS

```sh
target/loom check compiler/examples/http_upload_client
target/loom build compiler/examples/http_upload_client --output target/http-upload-client
target/http-upload-client https://localhost:8443/upload ./ca.pem ./document.bin
```

The server must accept chunked POST data and reply with status 200 and body
`accepted`. The native gate supplies an independent Rustls peer and certificate
authority on an ephemeral port. It checks binary contents, 8-KiB upload chunks,
separate trailers, header snapshots and source-error teardown at O0/O2 under GC
stress, alongside existing hostname/trust and response-limit tests.

The factory creates a fresh scoped file stream inside the upload Task. The
request uses `std.net.http.client.upload`, retaining verified TLS, URL policy,
concurrent response handling and a single five-second budget. The file is not
collected before sending. Changing the caller's header buffer after creating
the Task cannot change its synchronous metadata snapshot.

Missing files remain typed source failures; the example reports fixed diagnostic
text without echoing file paths, certificates, URLs or body data. Producers and
connections close on normal completion, failure and cancellation.
