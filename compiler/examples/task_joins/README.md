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
including empty and singleton tuples.
Two-argument `any(task_a, task_b)` and `race(task_a, task_b)` overloads
accept Tasks with the same result type and use the List join policies.

| `std.task` function | Awaited result | Rule |
| --- | --- | --- |
| `all(tasks)` | `List[T]` | Input order; the first observed fault fails the join and drains siblings. |
| `settled(tasks)` | `List[Outcome[T]]` | Input order; collect every outcome without early cancellation. |
| `any(tasks)` | `T` | First successful completion; fault if none succeeds. |
| `race(tasks)` | `Outcome[T]` | First terminal outcome. |

`all` and `settled` accept empty Lists; `any` and `race` fault on empty input.
An ordinary `Result.Err` is successful completion. No-result Tasks work too:
`all([sleep_ms(1), sleep_ms(2)]).await` produces a List whose logical length is 2,
and `any(...).await` has no value result. No source Unit spelling is needed.

`any`/`race` retire every loser before returning, including already-completed
producers that still own returned Tasks. A cleanup fault fails the join unless
a primary fault already won; the primary diagnostic stays authoritative.
Running blocking I/O can delay drain. Returned Task-valued results remain
one-shot obligations for the caller.

The Loom implementation registers each child once and selects indexed slots from
completion notifications. It does not rescan a container on each wake, create a
wrapper Task per input, or build a recursive join chain. Tuple `all` uses one
additional Task per group to extract the typed results after every child is
terminal. Native scheduling remains cooperative, not parallel. Tuple-await
syntax remains unimplemented.
