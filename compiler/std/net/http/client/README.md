# One-request HTTP/1.1 client

`std.net.http.client` performs one bounded, buffered HTTP(S) exchange using the
source URL/message libraries and scoped TCP/TLS acquisition. There is no HTTP
runtime or new dependency.
The [interoperability fixture](../../../../examples/http_client/README.md) uses
a real independent TLS peer; the native gate also runs the source package tests.

```loom
let response = std.net.http.client.get("https://example.com/data").await?
// response.status, headers, body and trailers use std.net.http.Response.
```

For other methods, build `request(method, url)`, update its headers/body and call
`send(request, options).await`. `options()` defaults to verified HTTPS, compiled
Mozilla roots, no client identity, a 30-second network budget, eight informational
responses and the message codec's byte limits. Custom PEM trust/client identity
is explicit. ALPN offers only HTTP/1.1; no insecure TLS verifier is supplied.

`send` validates and snapshots the encoded request and TLS buffers synchronously,
before queuing network work. Later caller mutations cannot alter that snapshot.
Host and the origin-form target come from the URL; fragments stay local.
Host/Connection/Upgrade/Proxy-Authorization are reserved; the client sends
`Connection: close`. Plain HTTP needs explicit `allow_http = true` and still
rejects Authorization/Cookie. This is library policy, not a language restriction
or a filter for secrets placed in arbitrary bodies/other headers.

Uploads and response reads overlap. A complete early final response can stop the
remaining upload; it does not wait for a rejecting peer to consume the body.
Informational responses share the configured count budget; the final response
retains separate trailers. Connections close after completion, failure or
cancellation; no idle connection is pooled. HTTP error statuses and redirects
are ordinary responses, not transport errors. Protocol switches and CONNECT
are unsupported.

The timeout starts after snapshot preparation and includes queuing, acquisition
and exchange. It requests cancellation and drains children; it is not a hard
return deadline for blocking OS lookups. Completed outcomes/cleanup faults retain
the Task deadline rules. TCP's own dial defaults still apply. Timeout or transport
failure does not prove that a server performed no action. The client never
retries, follows redirects, reads proxy environment configuration or forwards
credentials automatically.

`Error` contains typed enum causes, never a URL, certificate, header or response
body. Bodies stay binary/opaque; decompression, cookies, proxies, HTTP/2,
streaming uploads/responses and connection pools are separate library work.
The underlying [codec](../README.md) remains available for streaming or persistent
protocols. See [HTTP semantics](https://www.rfc-editor.org/rfc/rfc9110.html) and
[HTTP/1.1 framing](https://www.rfc-editor.org/rfc/rfc9112.html).
