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
document into a `Value`, not a lazy sequence of array items or JSON documents.

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
data. Document sequences remain open.

The [TCP example](../../../examples/json_stream/main.loom) sends UTF-8 and escapes
one byte at a time, exercises cancellation followed by a fresh parse, then
echoes the value through three-byte output chunks and decodes a typed record:

```sh
target/loom run compiler/examples/json_stream
target/loom test compiler/std/json/stream
```
