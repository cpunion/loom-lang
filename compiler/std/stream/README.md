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

`from_iter(create, input)` instead accepts an Iterator factory with the same
resource bounds. Lifting it does not make synchronous I/O nonblocking.
Wrappers retain MustScope and ordinary nested disposal, allowing further factory
composition. A factory error creates no owner; `take(..., 0)` still opens and
closes the source without pulling. The limit is checked before acquisition.

Async resource parameters are checked borrows, not ownership transfers. Their
calls must be directly awaited: `source.next().await` and `collect(source).await`
keep the owner alive until children drain, including cancellation and faults.
Storing, returning, joining or forwarding a borrowing Task rejects. An existing
scoped producer also cannot be copied into a returned adapter; use a factory.
NoSuspend resources still cannot cross suspension. Async acquisition returning
a MustScope value and general borrow-retaining adapters remain unsupported.
See the [file pipeline](../../examples/stream_lines/README.md).

[TCP chunks](../net/tcp/chunks/README.md) borrow an explicitly closed socket.
Synchronous algorithms should keep using `std.iter`, avoiding a Task per item.

Run the [example](../../examples/streams/README.md), or test the library with
`target/loom test compiler/std/stream`.
