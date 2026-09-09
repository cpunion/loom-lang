# Task outcomes and cancellation

```sh
target/loom check compiler/examples/task_outcomes
target/loom run compiler/examples/task_outcomes
target/loom test compiler/std/task
```

The program prints `outcomes finished`. `std.task.outcome(task).await` consumes
one Task and returns `Outcome.Completed(value)`, `Outcome.Faulted(error)` or
`Outcome.Cancelled`. `error.message` is owned Text containing the original
diagnostic, creation locations and test name when present. An ordinary
`Result.Err` is a completed value. No-result Tasks use `Completed(_)` in matches;
there is no explicit source `Unit` type.

`std.task.cancel(task)` consumes the handle and drains children, external work
and lexical cleanup before returning an outcome. It does not suspend: an active
blocking OS operation may delay the owner. A cleanup fault returns `Faulted`;
an already-completed Task retains its real result. Task-valued results still
must be consumed. OOM and unexpected runtime failures remain process faults.

These are single-task operations. See [dynamic joins](../task_joins/README.md)
for List `all`, `settled`, `any` and `race`. Tuple joins remain unimplemented.
