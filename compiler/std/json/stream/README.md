# JSON from async chunks

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

The [TCP example](../../../examples/json_stream/main.loom) sends UTF-8 and escapes
one byte at a time and exercises cancellation followed by a fresh parse:

```sh
target/loom run compiler/examples/json_stream
target/loom test compiler/std/json/stream
```
