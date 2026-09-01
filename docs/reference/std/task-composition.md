# Task composition

> Normative for Loom language version 0.4.

The standard Task source API provides policy-level composition over the
language's structured `Task[T]` values. These names are not keywords, operators,
or a public coroutine protocol:

- `Task.all` waits for every successful value;
- `Task.any` selects the first successful value;
- `Task.settled` observes every terminal state;
- `Task.race` observes the first terminal state.

The semantic boundary is a standard-library API declaration. HIR retains an
ordinary method call. The current version 0.4 implementation temporarily
resolves a canonical, unshadowed receiver namespace and member through an
embedded compiler-owned catalog to `TaskIntrinsic`, and only that private
identity may select a specialized fixed heterogeneous row. A local, parameter,
generic, imported or user-defined type, or third-party method with the same
spelling cannot be mistaken for it. Source spelling is never inspected by MIR
or code generation.

`TaskIntrinsic` is an implementation bridge, not part of the Task API or a
standard-library ABI. The completed library boundary declares these public
members in compiler-owned Loom source, resolves calls to their ordinary source
`DefId` values, and follows their bodies through normal reachability. Those
bodies may use inaccessible typed join/select, outcome extraction, timer, and
structured cancellation primitives. They are never mapped back to
`TaskIntrinsic`; the temporary catalog and enum are deleted when the general
source-level associated-function and tuple/List mechanisms can express this
surface. Evaluation order, types, cancellation, cleanup, and fault behavior
remain the API contract described here, and programs cannot name or depend on
the private substrate.

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
`race` require a nonempty List; awaiting an empty dynamic join raises
`EmptyTaskJoin`. A List result preserves input order, not completion order.

`List[Task[T]]` is an affine carrier: construction and `add` transfer child
handles, `length` borrows the carrier, and the join consumes it. Individual
Task extraction, copying, and nesting the carrier in another aggregate are not
available.

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

`Completed(T)` retains any recursive `MustScope` obligation in `T`. A File,
Socket, or aggregate containing one must move directly into a `scoped` binding
in that match arm; it cannot be ignored or discarded. For typed File and
Socket resources, the runtime transfers ownership from the completed child to
its owner Task, which may itself be the root Task, before retiring the child.
Faulted, cancelled, losing, and unconsumed tasks transfer no completed-result
handle. Terminal cleanup or typed result disposal closes their remaining built-
in handles before retired-task memory reclamation.

## Timer tasks

The Task API also provides:

```text
Task.sleep(milliseconds Int)  Task[Unit]
```

The `Int` duration must be nonnegative. `Duration`, and any other constrained
`Int`, reaches this boundary through the general implicit refined-to-base
conversion. A `Duration` has already established the constraint in ordinary
source code; `milliseconds` enforces it with a precondition. Raw sleep rejection
and deadline overflow fault as specified by the language's
[async and task rules](../language/async-and-tasks.md#timers-and-io-tasks).
