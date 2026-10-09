# Task joins

```sh
target/loom check compiler/examples/task_joins
target/loom run compiler/examples/task_joins
LOOM_TASK_COUNT=0 target/loom run compiler/examples/task_joins
LOOM_TASK_COUNT=20 target/loom run compiler/examples/task_joins
target/loom test compiler/std/task
```

The program prints `joins finished`. Its runtime count defaults to 5 and accepts
0–1000. List operations consume a `List[Task[T]]`; results keep their native type.
Tuple `all((task_a, task_b, ...))` and `settled((task_a, task_b, ...))`
accept any number of distinct result types and return a tuple in input order,
including empty and singleton tuples. A tuple of Tasks can also be awaited
directly as `(task_a, task_b).await`, with `all`'s policy and no import needed.
Two-argument `any(task_a, task_b)` and `race(task_a, task_b)` overloads
accept Tasks with the same result type and use the List join policies.

Tuple joins also accept fresh MustScope results. The example scopes a tuple
containing two leases and ordinary Text; resource fields are borrowed until
the tuple's block ends, then disposed in reverse order. Tuple `settled` keeps
only each active completed resource payload. Partial construction remains
protected during waits, faults and cancellation. `any` and `race` retain a typed
winning Task until every loser has drained, then transfer its fresh resource or
Outcome into the caller's scope. Dynamic List `all`/`settled` likewise retain
resource payloads in completed producers until selection finishes, then construct
their guarded result List in input order. The example also scopes a dynamic List
of leases, including an empty List when `LOOM_TASK_COUNT=0`.
It also scopes a fallible lease selected by `first_ok`: ordinary errors are
skipped, and the chosen lease closes only when the caller's block ends.

| `std.task` function | Awaited result | Rule |
| --- | --- | --- |
| `all(tasks)` | `List[T]` | Input order; the first observed fault fails the join and drains siblings. |
| `settled(tasks)` | `List[Outcome[T]]` | Input order; collect every outcome without early cancellation. |
| `any(tasks)` | `T` | First successful completion; fault if none succeeds. |
| `race(tasks)` | `Outcome[T]` | First terminal outcome. |
| `first_ok(tasks)` | `Result[T, List[E]]` | First Ok; otherwise input-ordered errors. |

`all` and `settled` accept empty Lists; `any` and `race` fault on empty input.
List `all` and `settled` declare and prove
`length(result) == old(length(tasks))` in Loom source. Generic callers can reuse
this normal-return guarantee, including for resource payloads; it grants no
content equality or facts about fault/cancellation outcomes.
An ordinary `Result.Err` is successful completion. No-result Tasks work too:
`all([sleep_ms(1), sleep_ms(2)]).await` produces a List whose logical length is 2,
and `any(...).await` has no value result. No source Unit spelling is needed.
`first_ok` accepts empty input as `Err([])`. Fresh MustScope Ok/error payloads
remain in their producer frames until selection finishes. Losing resources drain
before an Ok transfers; all errors transfer into one scoped List. Execution and
cleanup faults still fail the join, rather than becoming ordinary Result errors.

`any`/`race` retire every loser before returning, including already-completed
producers that still own returned Tasks. A cleanup fault fails the join unless
a primary fault already won; the primary diagnostic stays authoritative.
Running blocking I/O can delay drain. Returned Task-valued results remain
one-shot obligations for the caller.

The Loom implementation registers each child once and selects indexed slots from
completion notifications. It does not rescan a container on each wake, create a
wrapper Task per input, or build a recursive join chain. Tuple `all` uses one
additional Task per group to extract the typed results after every child is
terminal. Native scheduling remains cooperative, not parallel.
