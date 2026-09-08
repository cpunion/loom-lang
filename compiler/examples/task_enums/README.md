# Tasks in enum payloads

```sh
target/loom check compiler/examples/task_enums
target/loom test compiler/examples/task_enums
target/loom run compiler/examples/task_enums
```

The program prints `enums finished`. It transfers empty, single-Task and
multi-Task variants through completed producers, callbacks and dynamic methods,
then consumes the active payload with ordinary `match`.

```loom
import std.option.Option
async fn item() Int { 7 }
async fn main() {
    let value Option[Task[Int]] = Option.Some(item())
    match value {
        Option.Some(task) => { assert task.await == 7 }
        _ => {}
    }
}
```

Matching transfers the enum once. Each bound Task-bearing payload must be used;
`Option.Some(_)` is an error. Here `_` covers only `None`, so it is safe.
Even a known `None` is matched or transferred rather than discarded. At runtime
only the active variant's Tasks are adopted or retained; an empty variant creates
no Task. A named catchall binding keeps the whole enum's ordinary obligations.

`Result?` uses the same matching rules: it can extract a Task from `Ok` or return
a Task-bearing `Err` without awaiting it. Ordinary errors are values, not Task
faults. Task-bearing Lists and join APIs remain unfinished.
