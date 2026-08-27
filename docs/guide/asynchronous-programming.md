# Asynchronous programming

Loom implements structured asynchronous tasks with stackless coroutines. The
compiler lowers each `async fn` into a Loom MIR state machine and the native
runtime schedules it cooperatively. Source code does not implement a `Future`,
promise type, coroutine trait, or poll protocol.

`Task[T]` is the only name for a scheduled structured computation. `Promise`
and `Async` are not aliases, and Core does not expose a manually completed
promise primitive.

The executable companion is
[`examples/core03/tasks.loom`](../../examples/core03/tasks.loom).

## Async functions and suffix `.await`

An async function declares its logical result type:

```loom
async fn child() Int {
    7
}
```

Calling it produces `Task[Int]`; waiting for it produces `Int`:

```loom
let pending = child()
let value = pending.await
```

`.await` is a non-overloadable postfix keyword. It takes no parentheses and can
participate in a postfix chain:

```loom
let decoded = packet().await.decode()
let value = async_result().await?
```

Prefix `await task`, `.await()`, and `.await!` are not supported syntax. The
`?` is the ordinary `Result` propagation suffix applied after the task result;
it is not part of `await`.

Starting an async function creates and schedules a child task rather than
running its body synchronously on the caller's stack.

## Structured task ownership

A `Task[T]` is a compiler-known one-shot handle. Every live task must be awaited,
transferred to a supported synchronous carrier, returned, or consumed by a join
before its lexical owner exits. It cannot be silently ignored or explicitly
discarded, including when nested inside an aggregate.

The checker tracks this obligation over complete bindings and control-flow
paths. Repeated await, partial consumption, conditional consumption, and
overwriting a live task carrier are rejected when the compiler cannot prove one
complete transfer. This is a task-specific flow rule; it does not introduce a
general ownership or borrow language.

Core has no detached tasks and no user-callable cancellation API. A parent owns
its children. Parent failure, join-loser cancellation, or executor teardown
propagates cancellation and drains child cleanup before the operation finishes.

## Fixed joins return tuples

Use separate arguments for a fixed number of tasks. `Task.all` preserves the
individual result types and returns a tuple:

```loom
async fn label() Text {
    "loom"
}

let number, text = Task.all(child(), label()).await
```

The task expressions are evaluated from left to right and all children are
created before the parent waits. Results remain in input order, not completion
order. Parallel binding is ordinary tuple destructuring; it is not a second
multiple-return ABI.

## Dynamic joins return lists

Use a homogeneous list when the task count is known only at runtime:

```loom
var tasks = List[Task[Int]]()
for index in 0..worker_count {
    tasks.add(run_worker(index))
    Unit
}

let values = Task.all(tasks).await
```

The result has type `List[Int]` and preserves input order. A fixed list literal
also requests list-shaped output:

```loom
let values = Task.all([first(), second()]).await
```

Tuples and lists do not convert implicitly. A runtime-sized collection is
homogeneous; use an explicit enum or a common `dyn C` chosen at construction if
different concrete result shapes must share one list. Loom does not erase them
through a universal `any`.

## Join modes

All join functions construct a new task; they do not wait until the returned
task is explicitly awaited.

| Operation | Result for fixed arguments | Result for `List[Task[T]]` | Completion rule |
| --- | --- | --- | --- |
| `Task.all` | `Task[(A, B, ...)]` | `Task[List[T]]` | All tasks complete with values; a task fault/cancellation cancels and drains siblings |
| `Task.settled` | `Task[(TaskOutcome[A], ...)]` | `Task[List[TaskOutcome[T]]]` | Every task reaches a terminal state; one failure does not cancel the others |
| `Task.any` | `Task[T]` for one common `T` | `Task[T]` | First value completion wins; losers are cancelled and drained; no success raises `TaskAnyFailed` |
| `Task.race` | `Task[TaskOutcome[T]]` for one common `T` | `Task[TaskOutcome[T]]` | First terminal state wins; losers are cancelled and drained |

`TaskOutcome[T]` is a closed enum with these variants:

```loom
match Task.race(primary(), fallback()).await {
    Completed(value) => use(value)
    Faulted(fault) => report(fault.code(), fault.message())
    Cancelled => Unit
}
```

Matching must be exhaustive. `TaskFault` reports a task-local fault. A business
`Result.Err` is an ordinary completed value and is not reinterpreted as a task
fault. Process-level failures such as OOM do not become `TaskOutcome` values.

An empty list is valid for `all` and `settled`. `any` and `race` require a
non-empty input.

## Timers and I/O suspension

The compiler-known timer constructor returns a storable `Task[Unit]` value:

```loom
Task.sleep(10).await
```

`Task.sleep` accepts a non-negative millisecond `Int` or a `Duration`.
Constructing the timer returns immediately with a first-class task; the suffix
`.await` is the suspension point.
Loom source has no raw-handle readiness constructor. File and socket operations
expose typed tasks and preserve the scoped resource that owns the platform
handle.

The runtime implementation uses a generation-checked, one-shot
wait-registration ABI,
kqueue on macOS, epoll on Linux, and the `polling` crate's IOCP/AFD backend on
Windows. Notifications enqueue a ready task; they never re-enter a coroutine
directly on a callback stack. Pending tasks are registered with a real wait
source rather than busy-polled. Windows compilation is covered by target checks;
native Windows scheduling and I/O execution are gated by the configured Windows
CI job and are not claimed from a Unix cross-check. The raw ABI is an unsafe
runtime boundary, not a Loom language API; see the runtime internals for its
live-handle contract.

## Cleanup and cancellation

Each suspension point records the live coroutine state and active lexical
cleanup. Normal return, early return, `?`, task fault, and cancellation all run
registered `defer` and `scoped` cleanup in LIFO order.

An active `NoSuspend` scoped resource cannot cross `.await`. A `defer` block is
synchronous and cannot itself await. See
[Resources and cleanup](resources-and-cleanup.md).

## Current limits

The current Core intentionally does not provide:

- async methods or async concept requirements;
- async `defer` or general async destructors;
- detached tasks, callbacks, `then` chains, or user cancellation;
- user-defined coroutine/Future runtime protocols;
- multithreaded shared-memory execution;
- task-carrying arguments to async callables or task-carrying async results,
  because cross-frame task reparenting is not implemented.

These are compiler diagnostics, not behaviors to work around with erased values
or unchecked runtime operations.
