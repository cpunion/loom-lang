# Task fields and returned groups

From the repository root:

```sh
target/loom check compiler/examples/task_aggregates
target/loom test compiler/examples/task_aggregates
target/loom run compiler/examples/task_aggregates
```

The program prints `aggregates finished`. Tuples and records can contain Tasks,
including nested value fields. Reading a Task field transfers it; reading the
whole value transfers all its remaining obligations and requires every Task
field to be available. Ordinary metadata stays readable after Task fields move.

Tuple destructuring evaluates its input once. Repeated field reads, missing
consumption, inconsistent branches and overwriting live fields reject. Bind a
temporary aggregate before selecting one Task field when other Task fields would
otherwise be lost. No ownership, borrow or lifetime syntax is introduced.

Async parameters adopt every Task field. A producer retains every returned child
subtree until its consumer extracts the result, even if the completed producer
itself is transferred. The example exercises this with nested tuples, generic
forwarding, a callback, shared data and timers.

This is aggregate transfer, not a join API or tuple `.await` sugar. Await each
Task explicitly. Task-bearing Lists and enums remain unsupported; shared-container
transfer and multi-task completion observation are the next composition boundaries.
