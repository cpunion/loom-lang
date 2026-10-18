# Bounded UTF-8 line streams

`lines(source, bytes, maximum)` adapts any `Stream` to
`Stream[Item = Result[Text, LineError[E]]]`. The synchronous converter has type
`fn(S.Item) Result[Bytes, E]`, so transport errors remain typed.

```loom
import std.stream.Stream
import std.stream.lines.lines
import std.stream.from_iter
import std.iter.from_list
import std.result.Result
import std.text.to_bytes

fn raw(value Bytes) Result[Bytes, Text] {
    Result.Ok(value)
}

async fn main() {
    let chunks = from_iter(from_list([to_bytes("hé\r"), to_bytes("\nlast")]))
    let reader = lines(chunks, raw, 1024)
    discard reader.next().await
    discard reader.next().await
}
```

LF terminates a line; one immediately preceding CR is stripped. Bare CR, BOM
and NUL are ordinary content. Empty lines and final unterminated content are
retained, but a trailing LF adds no extra item. Strict UTF-8 decoding happens
after assembling the line, so code points can span chunks. Returned Text is an
isolated immutable copy.

`maximum >= 0` bounds payload bytes, not characters, excluding LF and its optional
CR. Zero admits only empty lines. `TooLong(maximum)` reports the configured limit;
`Utf8` reports invalid or incomplete encoding. A limit violation can be detected
before decoding. `Input(error)` preserves converter errors. These errors occur
once and permanently exhaust the adapter. Producer/converter faults and pull
cancellation also exhaust it, without replaying a partial line.

Construction does not pull, and each pull stops at its first line boundary;
there is no suffix prefetch. Internal storage retains one delivered chunk, at
most `maximum` payload bytes and a pending CR flag, not the complete input.
The producer controls chunk size. Source Bytes keep ordinary sharing; callers
must serialize pulls and mutations of retained chunks. Adapter copies share
their cursor; they are not independent readers or synchronization primitives.

Resource factories use the same convention as other stream adapters:

```loom
scoped reader = lines(create, input, bytes, 1024)
// Or: scoped reader = lines(open, input, bytes, 1024)?
// Async factory: scoped reader = lines(connect, input, bytes, 1024).await?
```

The factory runs once and transfers a fresh `Stream + Dispose + MustScope`
source. Existing scoped borrows cannot escape into returned adapters.
Factories returning `Task[S]` use `.await`; `Task[Result[S, E]]` uses `.await?`.
Acquisition does not pull, and cancellation drains the factory before cleanup.
Owned adapters dispose their source at scope exit, after pending child pulls drain.
A lazy source such as `std.file.chunks.stream` still opens only when pulled.

Run `loom test compiler/std/stream/lines` or the
[real TCP example](../../../examples/tcp_lines/README.md).
