# Streaming HTTP client

Count response-body bytes without collecting the document:

```sh
target/loom run compiler/examples/http_streaming_client -- https://example.com/
```

An optional second argument selects a PEM trust bundle instead of the built-in
Mozilla roots, useful with a local test server. The program prints the HTTP status
and byte count. HTTP status errors are still responses; transport/protocol errors
produce a generic diagnostic without echoing the URL or response body.

The awaited `consume` callback borrows a scoped `Incoming` response. It pulls
fresh body chunks of at most 16 KiB with backpressure and checks trailers after
completion. The default byte quotas and total network/consumer deadline still
apply. Returning early closes instead of draining the unread body; the response
cannot escape the callback or be captured by another Task.

The native HTTP-client gate runs this program against an independent Rustls
server at O0/O2 under moving-GC stress. This is HTTP/1.1 response streaming, not
request-body streaming, connection pooling or an HTTP server framework.
