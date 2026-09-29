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

`std.task.deadline.after_ns(task, duration).await` computes a deadline at the call;
`at(task, deadline).await` accepts a process-local monotonic timestamp. Both
request cancellation when the timer wins and return the actual `Outcome` after
drain. A completed result, including a returned Task, is never discarded to
manufacture a timeout. Cleanup failures remain `Faulted`. Running blocking OS
calls and cooperative scheduling may delay return past the deadline.

The policy is ordinary Loom source over `std.task.cancel_when(task, trigger)`.
That operation retires the trigger on early work completion, or requests work
cancellation when the trigger reaches any terminal state. The trigger is only a
signal: its result and existing failure are deliberately discarded, not returned
as the work's outcome. Newly raised trigger cleanup faults fail the waiter unless
an earlier work fault is already primary. Cancelling the waiter drains both
subtrees. Use `all`, `settled` or `race` when both Tasks represent result-bearing work.

See [dynamic joins](../task_joins/README.md) for List and tuple `all`/`settled`,
homogeneous `any`/`race`, and tuple `.await`.
