# JSON over async chunks

`parse(source, bytes).await` consumes one JSON document from a `std.stream.Stream`.
The synchronous callback has type `fn(S.Item) Result[Bytes, E]`, making the
producer's item/error conversion explicit. Success returns `Value`; failures
are `StreamError.Input(E)` or `StreamError.Syntax(ParseError)`.

Pulls and conversions execute once, serially. Empty chunks are not EOF. The
consumer waits for `None`, even after completing the root value, to reject
trailing input and retain later producer errors. A syntax/input error stops
before pulling the suffix. The incremental decoder retains the current scalar
token and result tree, not the complete document text. Strings, numbers,
duplicate keys and depth limits follow [the existing JSON policy](../README.md)
and the grammar in [RFC 8259](https://www.rfc-editor.org/rfc/rfc8259).

Cancellation drains a pending pull through ordinary Task cleanup; it does not
close the producer, undo consumed bytes or preserve the local decoder for a
restart. The caller closes borrowed resources after child Tasks drain. Input
size limits and deadlines belong to the caller/producer. This parses one
document into a `Value`, not a lazy sequence of array items.

`lines(source, bytes)` returns a `Lines[S, E]` Stream of LF-framed JSON documents.
Each `next().await` returns one `Result[Value, LineError[E]]` before the producer
reaches EOF. CRLF is accepted; blank/whitespace-only records reject. Empty input
is an empty sequence, a final LF adds no empty record, and a final valid record
may omit LF. Raw newlines inside a JSON string or container do not span records.
Arbitrary chunks may split UTF-8 or escapes, or contain multiple records. The
adapter retains the current chunk and decoder, not a complete sequence or Text.

`LineError.Input(E)` preserves producer errors. `Syntax(line, ParseError)` gives
a one-based record number and a byte offset within that record. The first error
is terminal and no later chunk is pulled; unread bytes of the delivered chunk
are discarded. Copies share a serialized cursor. Cancellation or a fault during
a pull/conversion is terminal for that cursor, with no automatic replay or
closure of a borrowed producer. Its pending child still drains normally.

`lines(create, input, bytes)` also accepts a fresh `Stream + Dispose + MustScope`
factory, including a fallible one. The returned adapter is then a scoped owner;
early exit and cancellation drain pending pulls before ordinary source disposal.
It cannot retain a borrow of an already scoped producer. Other document framing
remains open.

`decode(source, bytes).await` constructs a visible typed shape after the same
EOF check, using `std.json.from_value`. Infer the target from the result type:

```loom
let message Result[Message, TypedError[ReadError]] = decode(source, packet).await
```

`TypedError.Stream(StreamError[E])` preserves input/syntax errors;
`TypedError.Conversion(std.json.DecodeError)` preserves construction errors.
It retains the parsed tree during conversion, not a second complete Text.
Record fields, numbers, unsupported/private types and constraints follow the
ordinary typed decoder; no defaults or constraint bypass is introduced.

`write(value, chunk_size, sink).await` encodes a `Value` or supported typed shape
without preparing a full serialized Text or an intermediate typed `Value` tree.
The positive chunk size bounds fresh output buffers. The
callback has type `fn(Bytes) Task[Result[Int, E]]`; it must complete each whole
chunk before returning its byte count, as TCP/TLS `write_bytes` does. Calls run
serially, providing backpressure. Success returns the total byte count.

`OutputError.Sink(E)` retains a sink error; `Encoding(WriteError)` retains JSON
validation errors. `Count(expected, reported)` rejects partial, negative or
excessive success counts, without pulling another chunk or guessing a retry.
Already-emitted bytes are not undone, and cancellation drains the pending sink
without closing the caller's resource. Neither operation freezes shared input
data.

`write_lines(source, chunk_size, sink).await` writes a Stream of supported typed
values or `Value` documents as compact LF-framed JSON. Each document and its
final LF finish before the next producer pull, providing record-level
backpressure. Empty input writes nothing. The result uses `OutputError` and
counts all emitted bytes, including each LF; delimiters obey the same checked
full-chunk sink contract. An error or cancellation stops without requesting a
later record or retrying bytes that may already have escaped. There is no
implicit EOF/flush/close operation on the sink and no full-sequence buffer.
Producer error values require explicit handling before encoding, not an
implicit JSON representation of `Result`.

The [TCP example](../../../examples/json_stream/main.loom) sends UTF-8 and escapes
one byte at a time, exercises cancellation followed by a fresh parse, then
echoes the value through three-byte output chunks and decodes a typed record.
It also reads LF-framed records through an open connection, without waiting for
EOF, and checks terminal pull cancellation:

```sh
target/loom run compiler/examples/json_stream
target/loom test compiler/std/json/stream
```
