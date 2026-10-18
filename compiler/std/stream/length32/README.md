# Length-prefixed binary frames

`frames(source, bytes, maximum)` adapts a `std.stream.Stream` into a stream of
`Result[Bytes, FrameError[E]]`. The converter is `fn(S.Item) Result[Bytes, E]`.
Each frame has four big-endian length bytes followed by that many payload bytes;
the length excludes the header. Empty payloads are valid. `maximum` is explicit,
between zero and 4294967295, and bounds the announced payload length.

Headers and payloads may span arbitrary chunks, including empty chunks; one
chunk may contain multiple frames. A complete frame is returned without pulling
the next chunk or waiting for EOF. Every returned payload is a fresh buffer.
Copies share the cursor; serialize pulls and keep delivered input chunks stable
while the adapter consumes them.

The reader retains the current chunk and unfinished payload, not the sequence.
It rejects an excessive length before collecting payload bytes and does not
preallocate from an untrusted header. Bound the producer's chunks separately.
`collect` still accumulates all yielded frames; use a loop or short-circuiting
consumer when the sequence itself is unbounded.

`FrameError.Input(E)` preserves the producer/converter error. `TooLarge(size)`
reports the announced size. `TruncatedHeader(received)` and
`TruncatedPayload(expected, received)` distinguish EOF inside a frame from clean
EOF between frames. An error is yielded once, followed by `None`, without pulling
a later chunk. Cancellation or a fault during a pull/conversion is also terminal
for that cursor: pending children drain, but consumed input cannot be replayed.
A borrowed producer is not closed and no protocol resynchronization is guessed.

`frames(create, input, bytes, maximum)` accepts fresh
`Stream + Dispose + MustScope` factories, directly or through `Result`.
Async factories returning `Task[S]` or `Task[Result[S, E]]` use the same call
followed by `.await` or `.await?`. Acquisition does not pull; cancellation drains
the in-flight factory before consumer cleanup.
Use `scoped`; owned sources dispose once after pending pulls drain, including on
early exit and cancellation. An existing scoped source cannot be copied into a
returned adapter. Invalid limits fail before factory execution.

`encode(payload, maximum)` returns a fresh complete framed buffer, or
`EncodeError.TooLarge(size)`. It copies the current payload and leaves it unchanged;
it is not a streaming encoder or an atomic snapshot against concurrent mutation.
The reader imposes no text encoding or JSON policy.

The [TCP example](../../../examples/framed_tcp/main.loom) uses two-byte reads,
one-byte writes, an empty frame and non-UTF-8 bytes. It cancels a pending read
before sending, then reads messages while the write side remains open:

```sh
target/loom check compiler/examples/framed_tcp
target/loom run compiler/examples/framed_tcp
target/loom test compiler/std/stream/length32
```
