# Task callbacks

From the repository root:

```sh
target/loom check compiler/examples/task_callbacks
target/loom test compiler/examples/task_callbacks
target/loom run compiler/examples/task_callbacks
```

The program prints `callbacks finished`. An `async fn read(input Input) Int`
has the function-value type `fn(Input) Task[Int]`. A synchronous factory returning
`Task[Int]` has the same callable type: the factory runs inline, while an async
body starts only when scheduled.

Function values contain code, not pending Tasks. They can be copied, returned,
stored in records/Lists, and kept across awaits. Invoking them still requires an
async function or a synchronous helper with a Task-bearing parameter/result.
Actual Task arguments and results must be transferred or awaited exactly once.

The example covers contextual generic references, returned callbacks, function
fields beside concept methods, shared inputs, synchronous forwarding, and nested
Task results. No-result async callbacks currently use inferred types; `Unit` is
not a source type. This example uses named functions; [closures](../closures/README.md)
and [captured static callbacks](../comptime_closures/README.md) share their callable
shape. Compile-time construction may retain such references, but cannot actually
create, transfer or await Tasks. Lists of callbacks remain ordinary shared data;
[Lists of pending Tasks](../task_lists/README.md) use one-shot transfer operations
and [source join APIs](../task_joins/README.md).
