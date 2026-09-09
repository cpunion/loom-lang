# Dynamic Task lists

```sh
target/loom check compiler/examples/task_lists
target/loom test compiler/examples/task_lists
LOOM_TASK_COUNT=37 target/loom run compiler/examples/task_lists
```

The program prints `lists finished`. The optional environment variable chooses
0–1000 Tasks at runtime (default 5). It also checks empty groups, recursive
enum/List payloads, nested managed results and Unit-result Tasks.

`std.list.transfer.append` mutates and returns the same header. `take_last`
returns `Option[(T, List[T])]`: `Some` transfers the extracted element and
shortened list; `None` consumes the empty list. For example:

```loom
var pending = tasks
while true {
    match take_last(pending) {
        Option.Some(pair) => {
            let task, rest = pair
            pending = rest
            discard task.await
        }
        Option.None => { break }
    }
}
```

Ordinary Lists keep shared mutation. A Task-bearing List transfers once and
cannot be copied, discarded, or overwritten while live. Ordinary indexing,
get/push/set and length queries do not borrow that group. No source ownership
annotation is needed. This demonstrates transfer and draining, not `all`: prompt
sibling-fault observation and the join APIs remain separate work.
