# Task composition

> Normative for Loom language version 0.3.

The standard Task source API provides policy-level composition over the
language's structured `Task[T]` values. These names are not keywords, operators,
or a public coroutine protocol:

- `Task.all` waits for every successful value;
- `Task.any` selects the first successful value;
- `Task.settled` observes every terminal state;
- `Task.race` observes the first terminal state.

The semantic boundary is a standard-library API item. HIR retains an ordinary
method call. Version 0.3 resolves a canonical, unshadowed receiver namespace and
member through an embedded compiler-owned catalog, and only that stable item may
select a specialized fixed heterogeneous row. A local, parameter, generic,
imported or user-defined type, or third-party method with the same spelling
cannot be mistaken for that item. Source spelling is never inspected by MIR or
code generation. Future source-compiled standard-library declarations will map
their trusted definition identities to the same standard-item catalog; they do
not require new source syntax. Evaluation order, types, cancellation, cleanup,
and fault behavior
remain the source API contract described here, and programs cannot name or
depend on the private join substrate.

## Fixed task rows

Separate arguments preserve exact fixed types:

```text
Task.all(Task[A], Task[B])      Task[(A, B)]
Task.settled(Task[A], Task[B])  Task[(TaskOutcome[A], TaskOutcome[B])]
Task.any(Task[T], Task[T])      Task[T]
Task.race(Task[T], Task[T])     Task[TaskOutcome[T]]
```

Every fixed form requires at least one task. `all` and `settled` may be
heterogeneous. `any` and `race` require one common result type. Argument
expressions are evaluated from left to right before the returned join task can
be awaited. Tuple-shaped results preserve source order.

## Dynamic task lists

A runtime-sized join consumes one homogeneous List:

```text
Task.all(List[Task[T]])      Task[List[T]]
Task.settled(List[Task[T]])  Task[List[TaskOutcome[T]]]
Task.any(List[Task[T]])      Task[T]
Task.race(List[Task[T]])     Task[TaskOutcome[T]]
```

`all` and `settled` accept an empty List and produce an empty List. `any` and
`race` require a nonempty List; awaiting an empty dynamic join faults. A List
result preserves input order, not completion order.

A fixed List literal requests List-shaped output even when the compiler can
specialize its elements as one static row:

```loom
let values = Task.all([first(), second()]).await
```

## Completion and cleanup

| Function | Completion rule | Unfinished siblings |
| --- | --- | --- |
| `Task.all` | every child completes with a value | cancelled after a fault or cancellation |
| `Task.settled` | every child reaches a terminal state | allowed to finish |
| `Task.any` | the first successful value | cancelled after a winner; no success raises `TaskAnyFailed` |
| `Task.race` | the first terminal state | cancelled after a winner |

A join does not complete until every sibling it cancelled has finished lexical
cleanup. Cleanup uses the same block-scoped `defer` and `scoped` rules as an
ordinary task exit. A business `Result.Err` is a completed value and is never
reinterpreted as a task fault.

## Outcomes and faults

`TaskOutcome[T]` is the closed value:

```text
Completed(T)
Faulted(TaskFault)
Cancelled
```

It must be matched exhaustively. `TaskFault.code()` and
`TaskFault.message()` return Text describing the captured primary task fault.
Capturing a fault through `settled` or `race` makes it data; it does not resume
or rethrow the child. OOM remains a process-level fault and cannot become a
`TaskOutcome`.

## Timer tasks

The Task API also provides:

```text
Task.sleep(milliseconds Int)  Task[Unit]
Task.sleep(duration Duration) Task[Unit]
```

The duration must be nonnegative. Invalid duration or deadline overflow faults
as specified by the language's
[async and task rules](../language/async-and-tasks.md#timers-and-io-tasks).
