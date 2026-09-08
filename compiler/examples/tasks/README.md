# Typed task programming trial

From the repository root, after building the compiler:

```sh
target/loom fmt --check compiler/examples/tasks
target/loom check compiler/examples/tasks
target/loom test compiler/examples/tasks
target/loom run compiler/examples/tasks
target/loom build compiler/examples/tasks --output compiler/examples/tasks/target/tasks
```

The program prints `42 ready`; the package has two tests. Tests live beside the
code in `main_test.loom` and are excluded from a normal build.

`recorded[T]` returns a `Task[T]` when called. `joined` queues an integer task and
a text task before its first `.await`; their bodies do not run inline during
creation. Awaiting each task produces its own result type. Compare this with
`let total = recorded(42, order, 1).await`: the next child would not be created
until that await completes. The synchronous generic `forward` transfers a handle
without creating an executor. The async `render` function receives both tasks
and awaits their results in order; this is explicit composition, not a fail-fast
`Task.all` implementation.

This is a single-threaded CPU ready queue, not thread parallelism or an async
I/O implementation. The final `write_text` call is ordinary synchronous I/O.

Inside `render`, try duplicating `let label = word.await` with a different local name. `loom check`
rejects the second await. Replacing it with `discard word` also fails: a task
must be consumed. The second test shows a local transfer; `original` is no longer
usable after `let transferred = original`.

Use `.await`, not prefix `await` or `.await()`. Direct Task parameters and returns
are supported, including `Task[Task[T]]`. An async function with logical result
`Task[T]` yields a nested Task; each `.await` consumes one layer. A completed
producer retains its returned child until the actual consumer extracts it.
Calls evaluate arguments before transferring them, and early returns cannot
abandon already-evaluated Task arguments.

A synchronous helper that creates or transfers Tasks must expose a direct Task
parameter or return type; it uses its async caller's execution environment.
An ordinary synchronous `main` cannot start tasks. Task-containing aggregates,
async methods, Task-bearing function values/dynamic calls, and suspension with
active `scoped`/`defer` cleanup remain unimplemented. Task operations cannot run
inside cleanup; ordinary synchronous cleanup can precede a Task return.
