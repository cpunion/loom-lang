# Async streams

`Stream` has an associated `Item` and `async fn next(self Self) Option[Self.Item]`.
Generic and `dyn Stream[Item = T]` consumers use the same protocol. `None` ends a
consumer; an implementation need not promise that every later pull also ends.

```loom
import std.iter.range
import std.stream.from_iter
import std.stream.map
import std.stream.take
import std.stream.collect
import std.time.sleep_ms

async fn doubled(value Int) Int {
    sleep_ms(1).await
    value * 2
}

async fn main() {
    let source = map(from_iter(range(0, 10)), doubled)
    let values = collect(take(source, 3)).await
    assert values[0] == 0 && values[2] == 4
}
```

`from_iter` lifts an ordinary Iterator without copying its state. `map` and
`filter` accept callbacks returning Tasks. `take` admits a nonnegative limit;
its copies share their allowance, reserved before a pull. `collect`, `fold`,
`try_fold`, `any`, and `all` await one pull/callback at a time. Short-circuiting
does not prefetch the suffix. `try_fold` returns the first callback `Err`; Task
faults remain faults. Empty `any`/`all` return false/true.

Construction does not start pulling. Calling `next` creates an ordinary hot
Task, not a separate executor. Canceling a consumer drains its pending child
Tasks and lexical cleanup; it does not undo already consumed input, I/O, or
callback effects. A stopped cursor can be resumed when its producer allows it.
Callers must serialize pulls on shared cursors; this protocol supplies no
cross-thread or reentrant exclusivity guarantee.

## Scoped producers

Factory overloads of `from_iter`, `map`, `filter` and `take` create owned pipelines:

```loom
// create(input) returns Result[S, E], where S implements Stream, Dispose
// and MustScope. Each factory runs once, before any pull.
scoped source = take(create, input, 2)?
let values = collect(source).await
```

Factories may also return the fresh producer directly. These overloads return
the owned adapter directly, without `Result` or `?`:

```loom
// std.file.lines.stream is lazy; file errors are yielded by the first pull.
scoped source = take(stream, path, 2)
let lines = collect(source).await
```

`from_iter(create, input)` instead accepts an Iterator factory with the same
resource bounds. Lifting it does not make synchronous I/O nonblocking.
Factories can also return `Task[S]` or `Task[Result[S, E]]`, enabling asynchronous
acquisition and composition through the same adapters:

```loom
scoped source = take(connect_stream, address, 2).await?
let values = collect(source).await
```

Await acquisition before using the adapter. Cancellation drains an in-flight
factory before consumer cleanup; acquisition itself does not pull the source.
Wrappers retain MustScope and ordinary nested disposal, allowing further factory
composition. A factory error creates no owner; `take(..., 0)` still constructs
and disposes the source without pulling. An eager file factory opens/closes it;
a lazy one never opens it. The limit is checked before construction.

Async resource parameters are checked borrows, not ownership transfers. Their
calls must be directly awaited: `source.next().await` and `collect(source).await`
keep the owner alive until children drain, including cancellation and faults.
Synchronous `fn ... Task[T]` wrappers can return a Task borrowing only their
caller-provided resources; the caller must directly await the forwarding call.
Storing or joining that Task, returning a locally owned borrow, or exposing an
inner borrowing Task from an async function rejects. An existing scoped producer
also cannot be copied into a returned adapter; use a factory.
NoSuspend resources still cannot cross suspension. General borrow-retaining
adapters remain unsupported.
See the [file pipeline](../../examples/stream_lines/README.md).

[TCP chunks](../net/tcp/chunks/README.md) borrow an explicitly closed socket.
[`length32`](length32/README.md) turns arbitrary byte chunks into bounded binary
messages with a four-byte length prefix, including scoped producer factories.
[`lines`](lines/README.md) assembles bounded UTF-8 lines from arbitrary byte
chunks, including CRLF and code points split across chunks. It stops at each
line boundary and exhausts on errors or cancellation rather than replaying input.
Both framing adapters accept asynchronous owned factories; the
[TCP lines example](../../examples/tcp_lines/README.md) composes connection
acquisition, framing and an early-stop limit without manual socket cleanup in
the consumer.
[File chunks](../file/chunks/README.md) own a lazy file and yield bounded binary
buffers without text decoding or prefetching.
Synchronous algorithms should keep using `std.iter`, avoiding a Task per item.

Run the [example](../../examples/streams/README.md), or test the library with
`target/loom test compiler/std/stream`.
