# HTTP/1.1 messages

`std.net.http` reads and writes bounded HTTP/1.1 messages entirely in Loom.
It uses ordinary `Stream`, Bytes, records, generics and Tasks. There is no HTTP
runtime primitive or compiler special case. The framing rules follow
[RFC 9112](https://www.rfc-editor.org/rfc/rfc9112.html#section-6).

Run the [TCP application](../../../examples/http_loopback/README.md):

```sh
target/loom test compiler/std/net/http
target/loom test compiler/examples/http_loopback
target/loom run compiler/examples/http_loopback
```

## Data and encoding

`Request` holds a method, raw request target, headers, body and trailers.
`Response` holds a status, reason, headers, body and trailers.
`Header` stores an ASCII token name and an octet value; reason phrases and body
data also remain Bytes. Binary/legacy field values are not decoded as UTF-8.
`header(name, text)` is a checked convenience for UTF-8 field values.
Fields retain their order and spelling; surrounding field-value whitespace is
trimmed on parsing and encoding.

`encode_request(message, limits)` and `encode_response(message, method, limits)`
return `Result[Bytes, ProtocolError]`. Encoding validates privately copied fields
and body data. Later mutations of the inputs cannot change the returned bytes.
The encoder generates Content-Length when absent, checks explicit lengths against
the body, or emits chunked data when the caller specifies that transfer coding.
Trailers require chunked encoding and stay in their own section.
Pure encoding and parsing helpers work through ordinary compile-time execution.

## Streaming requests

`write_request(metadata, source, bytes, sink, limits).await` sends a request
without collecting its body. `RequestMetadata` holds `method`, `target`,
`headers` and `trailers`; `source` is a `Stream`, `bytes` converts each item to
`Result[Bytes, E]`, and `sink` is `fn(Bytes) Task[Result[Int, F]]`.
The [file upload example](../../../examples/http_upload/README.md) directly
borrows a scoped file stream and writes bounded chunks over TCP.

Absent explicit framing, the writer generates `Transfer-Encoding: chunked`.
An explicit Content-Length instead requires exactly that many body bytes;
premature EOF reports Truncated, excess data reports Framing. Length/coding
mixtures, duplicate framing and unsupported transfer codings reject.
Trailers require chunked framing and remain a separate terminating section.
Empty producer buffers are skipped, not emitted as a message terminator.

On Task execution, metadata and trailers are validated and snapshotted before
the first sink call or source pull. This is not a synchronous snapshot when the
Task is created. Each nonempty producer buffer is copied into a private output
chunk before awaiting the sink. The next pull waits for that sink to finish;
there is no whole-body buffer or prefetch. Choose bounded producer chunks to
bound memory. Body and total framing quotas apply across the entire message.

The sink must consume each complete buffer or return an error. The result is
`Result[Int, WriteError[E, F]]`: success counts wire bytes; Input/Sink preserve
their typed causes, Protocol reports codec failure, and Count reports an
incorrect sink byte count. No suffix is pulled or terminator emitted after an
error. Partial output is not undone or retried.
Faults and cancellation use ordinary Task child draining. The writer borrows
its producer and does not own the transport; the caller scopes resources and
closes a partially written connection on failure. Duplex response handling,
deadlines and early final responses remain caller policy. The source
client owns that policy rather than this codec.

For synchronous preflight, `request_writer(metadata, limits)` returns a checked
`RequestWriter`. Pass it to `write_request(writer, source, bytes, sink).await`.
Copies share one owner-thread cursor: once writing starts, a second invocation
reports Closed before any source/sink work, even if the first fails or is
cancelled. This is not cross-worker
synchronization or an automatic replay facility.

## Streaming responses

`write_response(metadata, method, source, bytes, sink, limits).await` uses the
same bounded sender as requests. `ResponseMetadata` holds `status`, `reason`,
`headers` and `trailers`; `method` is the corresponding request method.
Ordinary responses use an explicit Content-Length or chunked framing when no
length is supplied. Chunk snapshots, serial backpressure, quotas and typed
`WriteError` failures follow the request writer's rules.

HEAD, informational, 204, 304 and successful CONNECT responses stop after their
headers without pulling the producer or emitting a chunk terminator. Trailers
reject for these responses. HEAD/304 may retain representation length/coding
metadata; informational/204/successful CONNECT responses forbid those fields.
The caller still owns the unconsumed producer and the transport. Protocol
handoff and response ordering remain caller policy.

`response_writer(metadata, method, limits)` synchronously snapshots and validates
a `ResponseWriter`, including reason bytes. Its copies share one use, just like
`RequestWriter`; call `write_response(writer, source, bytes, sink).await`.
The [TCP example](../../../examples/http_loopback/main.loom) borrows a scoped
incoming request body directly into a streamed response without collecting
either body or assembling a complete encoded response.

## Reading

`reader(source, bytes, limits)` takes a `Stream` and a converter
`fn(S.Item) Result[Bytes, E]`. Construction does not pull. For example, inside an
async function with a caller-owned TCP stream:

```loom
// packet maps Result[Bytes, TcpError] to itself.
let input = reader(std.net.tcp.chunks.read(connection, 16384), packet, limits())
let response = read_response(input, "GET").await
```

`read_request(input)` and `read_response(input, request_method)` return
`Result[Option[Request/Response], ReadError[E]]`. `None` means clean EOF before
another message; EOF inside a framed message is an error. Transport/converter
errors retain their original type in `ReadError.Input`; protocol errors carry
no input bytes.

The reader supports Content-Length, chunked data with extensions/trailers, and
EOF-delimited response bodies. Requests without framing have no body. Response
framing uses the corresponding request method and status, so HEAD and bodyless
statuses do not consume the next response. Informational responses are returned
individually; the caller associates reads with outstanding requests and continues
until their final response, rather than treating unsolicited bytes as responses.
The codec neither retries requests nor interprets application status codes.

Copies share one cursor, including any unread suffix already in a producer chunk.
Only one message may own the cursor: a concurrent owner-thread read reports Busy
until that message's body completes, without disturbing it. This is not a
cross-worker synchronization primitive.
Parsing scans buffered bytes synchronously, with Tasks per line/chunk rather than
per octet. The producer is pulled only when buffered input is exhausted.
As with other returned adapters, an existing scoped producer cannot be captured
into this reader; use a caller-managed transport producer. Owning factory and
general borrow-retaining adapters remain separate work.

After an accepted protocol switch or successful CONNECT response, HTTP reads
stop. `take_buffered(input)` retires the reader and transfers a copy of its unread
octets; continue with those octets followed by the original producer. The codec
does not implement the new protocol or own that producer.

## Streaming bodies

`open_request(input, chunk_bytes)` and
`open_response(input, method, chunk_bytes)` stop after the headers and return
`Result[Option[IncomingRequest/IncomingResponse], ReadError[E]]`.
The incoming message and its `Body` implement MustScope: scope the result,
including when it is still wrapped in Result/Option. For example:

```loom
scoped incoming = open_response(input, "GET", 16384).await
```

The [TCP example](../../../examples/http_loopback/main.loom) shows the complete
match and consumption loop. `body.next().await` yields
`Option[Result[Bytes, ReadError[E]]]`. Each successful chunk is nonempty, fresh
and no larger than the positive `chunk_bytes` limit. Opening a message does not
pull its body; later pulls do not prefetch a suffix for the next pull.
The total body quota still applies across all chunks.

The body holds the reader's message lease. Reading another message or taking its
buffered suffix reports Busy until the current body completes. Fixed-length
bodies complete with their last data chunk; chunked bodies must also consume
the final chunk and trailers. `trailers(body)` is available only after successful
completion. A zero/bodyless message is complete at its headers.

Early scope exit retires the reader without draining an unwanted network body.
Errors, faults and cancellation drain pending child pulls and retire it too;
already delivered chunks are not rolled back. Cancelling a completed header
Task still cleans up any unclaimed body. A completed body's later cleanup does
not retire a reader already reused by another message. The body owns this parse
lease, not the transport; caller-managed transport cleanup remains necessary.

The buffered `read_request`/`read_response` functions collect this same body
stream rather than maintaining a second framing implementation. Streamed
requests and responses need no language/runtime extension or ownership syntax.

## Limits and failure policy

`Limits` sets `line_bytes`, `metadata_bytes` and `body_bytes`, all nonnegative.
`limits()` chooses 8 KiB per line, 64 KiB total framing metadata and an 8 MiB
decoded body. Line limits exclude CRLF; metadata includes start lines, headers,
chunk framing/extensions, trailers and their CRLFs. Integer parsing checks the
remaining body quota before arithmetic or allocation. These bound decoded
messages, not producer-owned chunk allocations; choose bounded input chunks too.

Parsing requires CRLF, rejects obsolete folding and whitespace before a field
colon, and requires exactly one Host field in requests. Duplicate/list
Content-Length values and length/transfer-coding mixtures reject instead of
choosing a potentially ambiguous interpretation. Only the `chunked` transfer
coding is supported; compression content codings remain opaque payloads.
HEAD/bodyless/tunnel response framing takes precedence over length fields.
Trailers never merge into headers; framing/routing field names reject there.

Failures, faults and cancellation retire the cursor and release retained buffers;
later HTTP reads report Closed. A Busy error leaves the active read alone.
After partial failure, close the transport rather than constructing another
reader and guessing a resynchronization point. The caller retains transport
ownership and must drain pending Tasks before cleanup. Deadlines are explicit
Task policy, not a hidden codec timeout.

This is a message codec, not a complete client/server or proxy. HTTP/1.0, HTTP/2,
general URI and field-specific validation, routing, redirects, cookies, content
decompression and connection pools are not supplied.
The separate pure [HTTP(S) URL parser](url/README.md) supplies a bounded outbound
host/port, Host authority and origin-form target; it does not interpret an incoming
request's routing policy or establish that its Host is authorized.
The [client](client/README.md) adds explicit TCP/TLS acquisition,
concurrent upload/response handling and deadlines while reusing this codec.
It can collect a response or stream bounded chunks through an awaited scoped
consumer; early return closes instead of draining the unread suffix.
Its separate GET/HEAD redirect operation resolves bounded same-origin hops;
redirect policy is not part of this codec.
Applications must validate Host/target semantics for their routing policy and
handle trailer-specific rules. TCP/TLS acquisition and trust remain separate;
this package does not weaken TLS verification or silently send credentials over
an unencrypted transport.
