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

Resource ownership is not transferred through these Tasks. MustScope producers
and owned resource pipelines need a separate safe boundary; they are not added
here. [TCP chunks](../net/tcp/chunks/README.md) borrow an explicitly closed socket.
Synchronous algorithms should keep using `std.iter`, avoiding a Task per item.

Run the [example](../../examples/streams/README.md), or test the library with
`target/loom test compiler/std/stream`.
