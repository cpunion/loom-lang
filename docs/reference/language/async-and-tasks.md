# Async functions and tasks

> Normative for Loom language version 0.3.

Loom async is structured and explicit. Core defines `Task[T]`, async functions,
postfix suspension, structured task obligations, and terminal task outcomes.
`Task.all`, `Task.any`, `Task.settled`, and `Task.race` are standard-library
policies rather than keywords, operators, or additional control-flow syntax.
Their callable signatures are specified in
[Task composition](../standard-library/task-composition.md).

## Async calls and `.await`

An async declaration writes its logical result type:

```loom
async fn load_count() Int {
    42
}
```

Its call and await types are:

```text
load_count()        Task[Int]
load_count().await  Int
```

`.await` is a postfix keyword, not an ordinary method and not overloadable. It
has no parentheses. Prefix `await task` is invalid syntax. Postfix chaining is
left to right:

```loom
let decoded = load_packet().await.decode()
let value = load_result().await?
```

Await is valid only inside an `async fn` or `test async fn`. It is forbidden in
a `defer` cleanup and while a `NoSuspend` scoped resource or active interface
access crosses the suspension point.

An omitted async return type is `Unit`, making the call type `Task[Unit]`.
Language version 0.3 has async functions and async tests, but no async method or
async concept-requirement declaration form.

## Task obligations

`Task[T]` is a one-shot structured obligation. Before every path leaves its
lexical scope, a task must be:

- awaited;
- transferred into one of the standard Task join operations;
- returned as the complete logical result of a synchronous callable; or
- transferred as a complete task-carrying value through a statically declared
  synchronous parameter or receiver that can assume the same obligation.

It cannot be silently ignored or passed through an unconstrained generic
boundary. Repeated consumption, consumption on only some control-flow paths,
wildcard loss, and overwriting a task-carrying place are static errors.

The obligation is recursive through tuples, lists, maps, options, results,
records, enums, task outcomes, and constrained wrappers. Whole-value structured
binding and exhaustive pattern matching can transfer that obligation. Partial
field extraction and task-carrying `List.get` or `TextMap` extraction are
rejected because version 0.3 does not expose partial ownership.

A Task-carrying value cannot be passed into an async callable, and an async
callable's logical result cannot itself contain a Task. These restrictions keep
the parent-child task relation explicit. Loom provides no detached spawn,
user-callable cancellation, or source-level task handle duplication.

## Standard Task joins

`Task.all`, `Task.any`, `Task.settled`, and `Task.race` form the standard Task
source API. They are not reserved syntax and user code cannot invoke the
compiler/runtime join protocol directly. The semantic boundary is a
standard-library API item, which implementations may specialize without
changing the source types, evaluation order, or fault semantics below. Version
0.3 resolves the canonical, unshadowed Task namespace and member through an
embedded compiler-owned catalog before applying its variadic type rule. Future
source-library declarations will map their trusted definition identities to the
same stable items. Same-spelled methods on ordinary values do not acquire Task
policy behavior.

A tuple of tasks can be awaited as one all-success operation:

```loom
let number, label = (load_count(), load_label()).await
```

The named join form makes the policy explicit and can be stored before waiting:

```loom
let combined = Task.all(load_count(), load_label())
let number, label = combined.await
```

For fixed arguments, `Task.all` preserves heterogeneous result types:

```text
Task.all(Task[A], Task[B])  Task[(A, B)]
```

`Task.settled` also accepts heterogeneous fixed arguments and produces a tuple
of corresponding `TaskOutcome` values. `Task.any` and `Task.race` require all
fixed arguments to have one common result type. Every fixed join requires at
least one task.

All task argument expressions are evaluated from left to right before the
parent waits. Tuple results remain in input order, not completion order.

## Dynamic task collections

For a runtime-determined task count, build a homogeneous list:

```loom
var tasks = List[Task[Report]]()
for index in 0..worker_count {
    tasks.add(run_worker(index))
}

let reports = Task.all(tasks).await
```

The dynamic signatures are:

| Operation | Result |
| --- | --- |
| `Task.all(List[Task[T]])` | `Task[List[T]]` |
| `Task.settled(List[Task[T]])` | `Task[List[TaskOutcome[T]]]` |
| `Task.any(List[Task[T]])` | `Task[T]` |
| `Task.race(List[Task[T]])` | `Task[TaskOutcome[T]]` |

The join consumes the complete task list. `all` and `settled` accept an empty
list and complete with an empty list. `any` and `race` require a non-empty list;
awaiting either operation with an empty dynamic list faults. The concrete fault
code is not yet a cross-backend compatibility guarantee.

## Join policies

| Operation | Completion rule | Other tasks |
| --- | --- | --- |
| `Task.all` | all tasks complete successfully | a failure cancels unfinished siblings |
| `Task.settled` | every task reaches a terminal state | a child failure does not cancel siblings |
| `Task.any` | first successful value | success cancels unfinished siblings; no success raises `TaskAnyFailed` |
| `Task.race` | first success, fault, or cancellation | unfinished siblings are cancelled |

A join does not return until cleanup for siblings it cancelled has completed.
Cancellation unwinds the cancelled task's active lexical scopes and runs their
`defer` and `scoped` cleanups in LIFO order.

A business `Result.Err` is a successfully completed task value. Join policy
does not interpret it as a task fault.

## `TaskOutcome` and `TaskFault`

`TaskOutcome[T]` is a closed value with three variants:

```text
Completed(T)
Faulted(TaskFault)
Cancelled
```

It must be matched exhaustively. `TaskFault` provides:

```text
fault.code() Text
fault.message() Text
```

`Task.settled` captures every child terminal state. `Task.race` captures the
first terminal state. Capturing a fault as `TaskFault` reports it as data from
the join; it does not add a source operation to resume or rethrow that task.

`Completed(T)` preserves every recursive `MustScope` obligation inside `T`.
If it contains a File, Socket, or another scoped resource, the successful arm
must bind that payload to `scoped` immediately; `_` and `discard` cannot erase
the obligation. For built-in File and Socket handles, the runtime transfers
ownership from a completed child to its owner Task, which may itself be the
root Task, before retiring the child. Faulted, cancelled, losing, and
unconsumed tasks do not transfer completed-result handles. Terminal cleanup or
typed result disposal closes their remaining built-in handles before retired-
task memory reclamation. User-defined `MustScope` obligations add no runtime
ledger entries.

## Timers and I/O tasks

The standard Task API provides:

```text
Task.sleep(milliseconds Int) Task[Unit]
Task.sleep(duration Duration) Task[Unit]
```

Sleep duration must be non-negative. A negative `Int` raises the
`InvalidSleepDuration` RuntimeFault; overflow while converting or adding the
monotonic deadline raises `SleepDurationOverflow`. The standard function
`std.time.milliseconds` constructs a non-negative `Duration`, whose
`.as_milliseconds()` method returns `Int`.

File and socket operations expose typed tasks. There is no Loom source
constructor that converts an `Int` into a raw readiness wait. See
[I/O and logging](../standard-library/io-and-logging.md).

## Result propagation and cleanup

`.await` and `?` are independent postfix operations. When an async operation
returns `Task[Result[T, E]]`, the common form is:

```loom
scoped file = try_open_read(path).await?
```

Await first obtains the Result, then `?` either produces `T` or returns the same
error type. Both error propagation and task cancellation run the lexical
cleanups described in [Memory and resources](memory-and-resources.md).
