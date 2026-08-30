# Async runtime

Loom async functions are stackless coroutines lowered from MIR into
compiler-generated state machines. A `Task[T]` is a one-shot asynchronous
computation, not a pull-based generator and not a wrapper around a host-language
future.

## Coroutine descriptor

The complete checked-MIR route uses a compiler-private descriptor containing:

- resume, cancel, and trace functions;
- value-slot and witness-slot counts;
- the result slot;
- state count and per-state live bitmaps.

Locals that survive a suspension are stored in the Task frame. The MIR
validator recomputes suspension liveness, and the GC traces only slots live in
the current state. Captured witnesses have separate slots and an owned proof
arena.

The first typed-LCIR coroutine slice uses the existing `typed-task-v1` runtime
wire with a different, exact compiler-shaped descriptor. LLVM target data lays
out one frame containing state, parameters, optional creation-site span
coordinates for state-zero preconditions, the ordered children and live values
for each suspension, and the result. The descriptor publishes frame
size/alignment, resume/cancel callbacks, result size/alignment, exact
managed-leaf byte offsets, and one live bitmap per resume state plus
completed-result state.
For a source coroutine, the same generated callback fills both the resume and
cancel descriptor entries. It first reads the Task's cancel-request bit, then
dispatches by the stored coroutine state. A normal invocation enters state zero
or the corresponding join-resume path. A cancellation invocation enters the
state's checked cancellation edge; state zero can terminate immediately because
there is no suspended lexical state to restore.
`Task[T]` itself is a stable scheduler-owned handle and is never a moving-GC
root. No universal value slot, witness arena, runtime type tag, or synchronous
expression executor is introduced by this route.

LCIR has explicit `TaskCreate`, `TaskSleep`, `TaskJoinAll`, `AwaitTasks`, and
`TaskOutcomeTake` operations. `AwaitTasks` stores its checked ordered child row,
join mode, and live row, registers one private structured join, and publishes
its state. The terminator has explicit normal, fault, and cancellation edges.
All three forward the same exact live-value row. The normal edge additionally
receives results for `all` and `any`, every terminal child handle for `settled`,
or the terminal winner handle for `race`. Each terminal handle is affine and
must be consumed immediately by `TaskOutcomeTake`, which constructs the exact
canonical `TaskOutcome[T]` at an explicit moving-GC safepoint. A propagating
child fault activates the source fault state, enters the compiler-expanded
static LIFO cleanup suffix, and ends in `ResumeFault`. Cancellation preserves
an inactive source-fault state, enters the same statically selected cleanup
suffix, and ends in `TaskCancelled`.
Cancellation cleanup cannot create, aggregate, or await Tasks, including
through an executor-dependent callee. It remains scheduler-topology neutral. If
cleanup faults after cancellation is established, the runtime keeps
cancellation primary and suppresses that cleanup fault while older actions
continue.

An already terminal join enters the corresponding checked edge immediately;
otherwise the callback returns pending and a completion notification puts the
parent back in the ready queue. A one-child await uses the same terminator with
one operand. Typed async run/test harnesses create one executor for the root
Task, drive it to a terminal state, take the exact result, and destroy the
executor. Cleanup is encoded entirely in LCIR control flow: this slice adds no
runtime cleanup stack, runtime symbol, or runtime ABI revision.

Current typed coverage includes coroutines without explicit mutable parameters
whose parameters, results, and live values use direct scalar/refined/product/Text
shapes, closed sums over those shapes, or concrete closed `List[T]` and
compiler-private `TextMap[V]` carriers. Each collection occupies one exact
managed-pointer frame cell; its repeated element graph remains in the ordinary
typed allocation descriptor rather than being embedded by value in the frame.
The same representation composes inside products and sums. Closed static Task
joins are described below. A synchronous callee may still use Loom's functional
inout ABI. The coroutine caller applies every normal or fault writeback to its
current SSA environment before continuing; on fault, this happens before the
coroutine's active lexical cleanup suffix. The coroutine itself does not expose
an inout Task result or alias the caller's storage.

A dynamic View parameter enters an async Task frame as an independent by-value
copy, even when the coroutine calls one of its mutable methods. Synchronous
mutable dispatch updates that frame-local copy, leaving the value used to create
the Task unchanged. When a reachable dynamic concept has exactly one closed
nongeneric witness, the planner erases the View recursively to its concrete
representation in coroutine parameters, results, products, sums, and live
suspension rows. Finite or open dynamic Views
do not acquire a coroutine-frame representation; here finite means a catalog
with multiple exact witnesses rather than the erased unique-witness case.

Lexical `defer` and admitted `scoped` resources may remain active across
suspension. The collision-free carrier gives managed sums one static union of
exact pointer offsets, and pack leaves inactive pointer lanes zero. This applies
equally to coroutine parameters, suspension rows, completed Task results, and
exact stored-join tuple results without changing typed-task v1. A fallible
callback creates one activation-local fault context attached to its executor.
Checked arithmetic, assertions, ordinary fallible invokes, async state-zero
preconditions, and callee-side postconditions record only the first fault on the
active Task. An async precondition reads its creation-site span from the Task
frame; a root Task receives its declaration span from the harness. Await
propagates a child's `Faulted` or `Cancelled` state; it never
converts either state into a source `Result`. Task handles may be live only as
suspension bookkeeping.

For reachable graphs with no LCIR-only primitive, selected async functions with
explicit mutable coroutine parameters, raw readiness, dynamically sized or
computed-List Task joins, `Task.any`,
`Task.settled`, or `Task.race` whose result is stored or otherwise used
first-class, and finite-catalog or open dynamic-concept frame values still
select the complete checked-MIR route. Async roots with `requires` use the same
typed state-zero check as child Tasks.

## Runtime and executor

`LoomRuntime` owns the managed heap. A `LoomExecutor` borrows one runtime for
its entire lifetime, and only one executor may attach to a runtime at once.

The executor is single-threaded and contains:

- Task storage and a FIFO ready queue;
- the active Task and parent/child ownership;
- join specifications and retired-task reclamation;
- an optional OS reactor;
- an optional mailbox for blocking I/O completions.

The reactor and worker mailbox are initialized lazily. Pure synchronous native
programs do not create them, and async programs that never suspend on external
readiness need not create the reactor.

## Wait ABI and reactor

The versioned wait ABI (`wait-v1`) defines:

- `LoomWaitSource` for timers, opaque platform I/O readiness, and worker
  completions;
- `LoomRegistration` with a key and generation;
- one-shot `LoomReadyNotification` records;
- cancellation and stale-registration rejection.

The runtime uses the Rust `polling` crate, which maps to epoll on Linux, kqueue
on macOS, and its IOCP/AFD implementation on Windows. The ABI transports only
opaque 64-bit handle bits; Unix descriptor and Windows socket ownership types
remain private to the platform adapter. Notifications carry the suspended
frame identity and enqueue the corresponding Task for another resume step.

This ABI is not exposed as a Loom source constructor. Direct registration is
an unsafe runtime boundary: an I/O source's bits must identify a live, valid
handle of the expected kind for the current platform, its interests must match
that handle, and the owner must keep it open and unreused until the one-shot
registration is delivered or cancelled. Values such as Unix standard-stream
descriptor numbers are never portable handles. Runtime typed file/socket tasks
and the typed `WaitSet` adapter establish this contract without exposing raw
integer handles to Loom programs.

The public safe `WaitSet` utility duplicates registered I/O handles, so a
caller closing or reusing its source cannot invalidate the registration.
Compiler-generated resource tasks likewise retain or duplicate the native
resource according to the scoped operation's ownership contract.

## Typed timer tasks

Checked `Task.sleep` is admitted wherever LCIR has a proved current executor
context. This includes an async callback and a synchronous helper reached from
one. The LCIR terminator consumes canonical `Int` milliseconds; lowering
extracts the single field from a source `Duration` before reaching that
boundary. It has explicit normal and fault edges and contributes `MAY_FAULT`
plus `NEEDS_EXECUTOR`, but it does not suspend or collect. A negative input
raises `InvalidSleepDuration`.
Signed millisecond-to-nanosecond multiplication and unsigned monotonic-deadline
addition are checked separately; either overflow raises
`SleepDurationOverflow`.

On the normal edge LLVM calls
`loom_typed_timer_task_create_v1(executor, deadline_ns)`. This narrow factory
creates and publishes a zero-root, zero-sized-result typed `Task[Unit]`. Its
runtime-owned callback registers the existing timer `WaitSource`, returns
pending, and publishes Unit when the one-shot notification resumes it. A
registration failure records `TimerRegistrationFault` as the Task's primary
fault. Cancellation removes an outstanding registration, and its generation
prevents a stale ready notification from re-enqueuing the cancelled Task.

The factory is the current `typed-timer-v1` boundary. The timer carries no
universal value or moving-GC root and reuses the typed-task and wait protocols.
The complete current identity is defined in
[Versioning and compatibility](../project/versioning.md).

Synchronous Task-producing helpers receive the active executor through one
effect-derived compiler-private parameter. The parameter follows any hidden
fault context, so a fallible helper has the physical order `source arguments,
fault context, executor`. Calls forward it transitively. A helper may create a
child Task, a fixed `Task.all` composite, or a timer Task, but it cannot await,
drive the queue, create a nested executor, or outlive the async structured
scope. Checked artifact roots reject a synchronous root that would need this
capability.

## Private primitives for static standard-library joins

The source names `Task.all`, `Task.any`, `Task.settled`, and `Task.race` define
standard-library policies. Source lowering emits ordinary method-call HIR.
Semantic resolution first excludes local, parameter, and module value bindings,
imports, generic parameters, and user types, then maps a canonical Task member
through the version 0.3 embedded standard-item catalog. Type checking and MIR
specialization consume only that stable identity. Source-compiled standard
library declarations will replace this catalog lookup with trusted definition
identity; the downstream boundary does not change.
The private substrate exposes typed join/select readiness, exact result or
outcome extraction, and structured cancellation-and-drain. It does not make the
four public policy names source-language constructs. `AwaitMode` is an internal
execution representation, not a public coroutine protocol.

Inside an admitted async function, a nonempty fixed argument list preserves its
heterogeneous child outputs as `Task[(T0, ..., Tn)]`. Children are evaluated
left to right. An immediately awaited fixed tuple or `Task.all` lowers directly
to one multi-child `AwaitTasks`; no intermediate composite Task is allocated.
The resume edge receives one exact result per child in task order and constructs
the source tuple, including the canonical one-field tuple for one child.

A first-class stored fixed `Task.all` instead lowers to `TaskJoinAll`. LLVM
generates an exact composite frame holding state, the ordered child handles, and
the target-laid-out result tuple. Its immutable typed-task descriptor traces
only the managed leaves of the completed tuple. Identical static result shapes
share one generated descriptor and resume callback. That callback uses the
ordinary structured `all` protocol, takes each exact child result, builds the
tuple, and publishes it without a universal join-result buffer or runtime type
tag.

Construction initializes the composite while it is unpublished, then calls
`loom_typed_task_publish_adopting_v1(executor, composite, children, count)`.
The runtime validates the complete ordered transfer and completes every
fallible reservation before changing ownership or queue topology. Success moves
the selected children from the active parent under the composite and publishes
the composite atomically; failure leaves the original topology unchanged, so
generated code can abort the unpublished frame and fail closed.

This is the current `typed-task-adopt-v1` boundary and composes with the
typed-task, timer, wait, and GC protocols without a second Task representation.

A nonempty, immediately awaited, fixed-arity `Task.any` is also direct when
every child has the same exact output type `T`. Its `AwaitTasks` plan retains one
output entry and one frame child field for every source child, while the normal
continuation receives only the successful winner's `T`. No first-class
composite Task or universal join-result buffer is created.

The ordinary scheduler selects the first successful child, requests
cancellation of unfinished losers, and drains the complete child set. When the
source callback consumes the resulting completion token through
`loom_task_join_step`, the typed runtime finalizes the join exactly once. It
disposes completed loser results and retires every loser in static reverse-input
order. A loser-disposal fault changes the await outcome to fault before the
coroutine enters its static LIFO cleanup. If every child fails or is cancelled,
generated code records the canonical `TaskAnyFailed` fault at the await's source
origin before entering the same fault cleanup. Cancellation of the source
coroutine bypasses normal join finalization, preserves cancellation as primary,
and relies on ordinary terminal child retirement; an established cancellation
continues to suppress a cleanup fault.

The winner keeps its original input ordinal and remains attached until generated
code switches over the coroutine frame's original child fields and performs the
exact typed result take. Loser retirement may therefore shrink the runtime join
list without changing result selection.

`Task.settled` and `Task.race` use the same static child rows. `settled` waits
for every terminal child and injects all child handles in source order. `race`
selects the first terminal child, cancels and drains the losers, and injects
only the winner handle. The compiler emits one `TaskOutcomeTake` for each
injected handle. Its `loom_typed_task_take_outcome_v1` boundary moves an exact
completed value, allocates independently rooted managed Text for a fault code
and message, or records the payload-free cancelled variant, then detaches and
retires the child. LLVM constructs the canonical closed `TaskOutcome[T]` value
from that status and the exact payloads. Each capture is an explicit collecting
safepoint, so outcomes already constructed remain in the ordinary exact root
plan while a later fault is captured.

Successful completion consumption preserves typed File/Socket ownership
before that retirement. `loom_typed_task_take_result_v1` moves those concrete owners to
the task's non-null owner Task before detaching and retiring the child. When it
is applied directly to the ownerless root Task, the owners remain on that
Task's `owned_result_resources` ledger in the executor-owned task registry.
The Completed branch of `loom_typed_task_take_outcome_v1` always consumes a
child and moves its resources to that child's owner Task, which may itself be the
root Task. The transfer is part of the successful commit: validation failure
leaves both task topology and ownership unchanged, and a repeated take cannot
duplicate ownership. Faulted and cancelled outcomes transfer no completed-
result resources. Fault/cancellation cleanup and completed loser or unconsumed
result disposal release all remaining typed File/Socket ledger entries at
that deterministic cleanup boundary, even if a result disposer reports a fault
or protocol defect. Retired-task reaping then reclaims memory only.

For a child take, preflight independently requires exactly one entry in the
owner's structured-child row and current join row, a settled successful join,
and the matching policy shape: result take is valid only for ALL/ANY, while
outcome take is valid only for SETTLED/RACE. ANY/RACE also requires winner
finalization to have completed before the winner can be removed. This prevents
an early or wrong-shaped take from changing winner ordinals, retaining stale
Task pointers, or bypassing loser disposal.

Both `any` and `race` use winner finalization when the source
callback consumes `loom_task_join_step`. It retains the original winner,
disposes completed loser results, and retires losers in static reverse-input
order. A loser-disposal fault becomes primary before source cleanup. The
current private identity names `typed-task-winner-finalize-v1`,
`typed-task-outcome-v1`, and `typed-resource-ownership-v1`. Completed-result
take transfers resource-ledger ownership before retiring the child; all other
terminal outcomes retain no deliverable resource. Runtime bundles must match
the complete current identity even when exported function shapes agree.

A sole nonempty List literal is also a closed static row. Lowering consumes its
elements directly without allocating the input List. `all` and `settled`
construct the requested output List after resume; `any` and `race` retain their
scalar result. Empty, stored, computed, and runtime-sized Lists stay outside
this fixed-row boundary.

## Typed File and Socket tasks

All native File and Socket operations use typed LCIR. The seven runtime leaves
cover File open/create/read/write and Socket connect/read/write. LCIR records
whether the source API returns `Task[Result[T, IoError]]` or faults as
`Task[T]`; this mode changes only the compiler-generated frame and completion
callback. It does not change the runtime request or outcome wire.

`loom_typed_io_task_create_v1` copies every borrowed Text range and duplicates
an input resource before retaining work. `loom_typed_io_poll_v1` reports only a
primitive resource token, Text scratch root, Unit, or closed error kind, fault
class, and message. The fault class preserves `InvalidPort` and
`SocketResolveFault` separately from `SocketConnectFault`; every other host
failure uses its operation-specific fault. Generated code then constructs the
exact recoverable result, publishes the exact faulting-family success value, or
records that Task fault. `loom_typed_io_cancel_v1` supplies non-suspending leaf
cancellation, and `loom_typed_resource_close_v1` performs final kind-checked
lexical release.

The runtime retains `typed-io-v1`, `typed-resource-v1`, the worker mailbox, and
both completion and platform-I/O `WaitSource` kinds. It no longer exports
universal `loom_file_*`, `loom_socket_*`, or `loom_io_close` functions and no
longer shares compiler-fixed File/Socket nominal IDs. Consequently, a reachable
I/O graph that cannot lower completely through LCIR fails native preparation;
the checked-MIR route cannot reinterpret it through a universal Task result.

## Blocking I/O

Operations that cannot use readiness directly are submitted to a process-wide
bounded blocking pool:

- four named worker threads;
- a bounded queue of 256 jobs;
- per-executor completion delivery through the lazy worker mailbox and reactor
  completion source.

Task cancellation invalidates readiness registrations immediately. It
atomically claims a queued blocking job, drops the still-unstarted host closure
and its captured resources, and makes the Task runnable without waiting for a
worker. The eventual empty queue entry performs no work and emits no
completion. A job which already started must finish and publish its private
completion; the runtime discards the result and only then schedules the Task's
cancellation callback. Executor teardown applies the same distinction before
releasing Task or reactor state. Generation checks prevent a late notification
or worker completion from waking a reused Task registration.

## Scheduling, completion, and faults

Internal Task states are runnable, running, waiting, draining, completed,
faulted, and cancelled. A Task executes until it completes, faults, or reaches
an explicit suspension. Completion notifications place work back in the ready
queue; the scheduler does not repeatedly poll every pending Task.

Tasks form a structured parent/child tree. The parent owns spawned children and
drains or cancels them before terminal completion. Lexical cleanup continues
while a task drains. The first fault recorded for a Task remains the primary
fault; a later cleanup fault does not overwrite it.

Runtime faults do not become ordinary source `Result` values. Direct calls,
`.await`, and `Task.all` propagate a child fault. A `Task.any` with no successful
child raises `TaskAnyFailed`; a fault raised while disposing a completed loser
instead remains primary. `Task.settled` and `Task.race`, whose purpose is to
observe terminal child states, represent the same fault as `TaskFault`; this
includes an artifact construction-proof replay failure. OOM alone remains
process-level and is not a Task terminal value.

The runtime join protocol supports the standard library's tuple and list forms
of:

- `Task.all`;
- `Task.settled`;
- `Task.any`;
- `Task.race`.

Tuple inputs preserve heterogeneous result types. List inputs support a dynamic
number of homogeneous tasks. A successfully consumed completed result transfers
its concrete typed File/Socket owners to the child's owner Task, which may
itself be the root Task, before the child is retired or reclaimed. Faulted,
cancelled, losing, and unconsumed tasks do not transfer completed-result
resources. Their terminal cleanup or typed result disposal releases remaining
built-in owners before retired-task memory reclamation.

The complete runtime and checked-MIR compiler route implement all of those source
forms. The typed-LCIR route admits nonempty immediately awaited fixed-argument
forms of all four APIs and a sole nonempty List literal. `all` and `settled`
may preserve heterogeneous fixed outputs; `any` and `race` require one exact
output type. A stored fixed `Task.all` also has an exact composite Task. Empty,
stored, computed, or runtime-sized List joins and first-class results of the
other three APIs remain atomic whole-artifact fallback.

## Current limits

The current runtime is process-local and single-thread scheduled. It is not a
shared-memory work-stealing executor. Task ownership transfer across arbitrary
async call boundaries, a general user-facing cancellation handle, generators,
streams, actors, persistent coroutines, and remote execution are not
implemented language features.

macOS arm64 runs the native async/standard-library tests in the development
gate. The release matrix owns Linux and Windows closure; its Windows entry is
not verified runtime or release evidence until a successful archive run is
recorded.
