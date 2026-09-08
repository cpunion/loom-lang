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
not a source type. These are named functions, not capturing closures. Compile-time
evaluation still rejects references to Task-creating/transferring functions.
Lists of callbacks work; Lists of pending Tasks and join APIs remain unfinished.
