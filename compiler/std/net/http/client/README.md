# One-request HTTP/1.1 client

`std.net.http.client` performs one bounded HTTP(S) exchange using the
source URL/message libraries and scoped TCP/TLS acquisition. There is no HTTP
runtime or new dependency.
The [interoperability fixture](../../../../examples/http_client/README.md) uses
a real independent TLS peer; the native gate also runs the source package tests.
`get`/`send` collect a response; `consume` streams it through an awaited callback.

```loom
let response = std.net.http.client.get("https://example.com/data").await?
// response.status, headers, body and trailers use std.net.http.Response.
```

For other methods, build `request(method, url)`, update its headers/body and call
`send(request, options).await`. `options()` defaults to verified HTTPS, compiled
Mozilla roots, no client identity, a 30-second network budget, eight informational
responses and the message codec's byte limits. Custom PEM trust/client identity
is explicit. ALPN offers only HTTP/1.1; no insecure TLS verifier is supplied.

`send` and `consume` validate encoded requests and snapshot request/TLS buffers
synchronously, before queuing network work. Later caller mutations cannot alter
that snapshot.
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
and exchange, including the awaited consumer. It requests cancellation and drains
children; it is not a hard return deadline for blocking OS lookups.
Completed outcomes/cleanup faults retain
the Task deadline rules. TCP's own dial defaults still apply. Timeout or transport
failure does not prove that a server performed no action. The client never
retries, follows redirects, reads proxy environment configuration or forwards
credentials automatically.

## Streaming responses

`consume[T](request, options, handler).await` returns `Result[T, Error]`; omitting
`options` selects the defaults. The handler has type
`fn(Incoming) Task[Result[T, Error]]`. The
[streaming example](../../../../examples/http_streaming_client/README.md) counts
bytes without collecting the document and returns the status/count as a tuple.

The client scopes `Incoming` before borrowing it into the directly awaited
handler. Use `status`, `reason` and `headers` for metadata, `.next().await` for
fresh binary chunks of at most 16 KiB, and `trailers` after successful completion.
The response cannot escape the handler, be copied or captured by another Task.
There is no background body drain or automatic replay of the callback.

The handler controls consumption. Returning early closes the connection without
draining the unread body; success means the handler succeeded, not that an unread
suffix was valid. Full body/trailer validation requires consuming to completion.
`send` is the collecting handler over this same path and performs that validation.
Handler errors, faults, cancellation and timeout drain pending children before
transport cleanup. Byte quotas and the one deadline apply throughout; streaming
does not remove the default body limit. `send`/`consume` snapshot and buffer their
request bodies. Application-specific errors can be returned inside `T`.
Resource-valued consumer results are not supported by this adapter; `T` is
ordinary result data, not an ownership transfer of the scoped input.

## Streaming uploads

Use `upload_request(method, url)` to create `UploadRequest` metadata (headers and
separate trailers), then `upload(value, options, create, input, bytes).await`.
Omit `options` for defaults. `consume_upload` adds the same awaited `Incoming`
handler as `consume`, returning ordinary `T` instead of collecting the response.
See the [HTTPS file upload](../../../../examples/http_upload_client/README.md).

`create` is `fn(A) Result[S, E]`, with `S Stream`; `input` is ordinary capturable
factory data, and `bytes` is `fn(S.Item) Result[Bytes, E]`. The factory runs inside
the upload child after acquisition. Its result enters a fresh lexical owner:
MustScope payloads are disposed, ordinary data keeps its sharing semantics.
An already scoped source cannot be captured or copied into that factory.
The factory is synchronous; async file/network work belongs in the producer's
pulls. A constructor failure is an ordinary Input error, not a Task fault.

The client validates request metadata and snapshots metadata/TLS buffers
synchronously before queuing network work. It does not pre-read the producer: each chunk is privately
copied and fully written before another pull. Choose bounded producer chunks;
the total body/framing quotas still apply. Missing framing selects chunked;
explicit Content-Length must match exactly. Trailers are snapshotted up front.

The existing concurrent response path, informational limit and one deadline
apply. A successful handler/final response may stop an unfinished upload; it
does not certify that the entire producer was read. Source failure retires the
transport, waking pending response I/O. Cancellation/fault/timeout drains source
pulls and handlers before connection cleanup. No retries or replay are added.

Results use `UploadError[E]`, with `Input(E)` and `Client(Error)` variants.
The client-generated `Error` contains typed enum causes, never a URL,
certificate, header or response body; custom `E` remains the application's data.
Bodies stay binary/opaque; decompression, cookies, proxies, HTTP/2 and connection
pools are separate library work.
The underlying [codec](../README.md) remains available for streaming or persistent
protocols. See [HTTP semantics](https://www.rfc-editor.org/rfc/rfc9110.html) and
[HTTP/1.1 framing](https://www.rfc-editor.org/rfc/rfc9112.html).
