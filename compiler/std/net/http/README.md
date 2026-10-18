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
Only one read may be active: a concurrent owner-thread read reports Busy without
disturbing the first. This is not a cross-worker synchronization primitive.
Parsing scans buffered bytes synchronously, with Tasks per line/chunk rather than
per octet. The producer is pulled only when buffered input is exhausted.
As with other returned adapters, an existing scoped producer cannot be captured
into this reader; use a caller-managed transport producer. Owning factory and
general borrow-retaining adapters remain separate work.

After an accepted protocol switch or successful CONNECT response, HTTP reads
stop. `take_buffered(input)` retires the reader and transfers a copy of its unread
octets; continue with those octets followed by the original producer. The codec
does not implement the new protocol or own that producer.

## Limits and failure policy

`Limits` sets `line_bytes`, `metadata_bytes` and `body_bytes`, all nonnegative.
`limits()` chooses 8 KiB per line, 64 KiB total framing metadata and an 8 MiB
buffered body. Line limits exclude CRLF; metadata includes start lines, headers,
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
URL/authority and field-specific validation, routing, redirects, cookies, content
decompression, connection pools and streaming body delivery are not supplied.
Applications must validate Host/target semantics for their routing policy and
handle trailer-specific rules. TCP/TLS acquisition and trust remain separate;
this package does not weaken TLS verification or silently send credentials over
an unencrypted transport.
