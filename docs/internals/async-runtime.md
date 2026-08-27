# Async runtime

Loom async functions are stackless coroutines lowered from MIR into
compiler-generated state machines. A `Task[T]` is a one-shot asynchronous
computation, not a pull-based generator and not a wrapper around a host-language
future.

## Coroutine descriptor

The complete legacy route uses a compiler-private descriptor containing:

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
out one frame containing state, parameters, the child and live values for each
suspension, and the result. The descriptor publishes frame size/alignment,
resume/cancel callbacks, result size/alignment, exact managed-leaf byte
offsets, and one live bitmap per resume state plus completed-result state.
`Task[T]` itself is a stable scheduler-owned handle and is never a moving-GC
root. No universal value slot, witness arena, runtime type tag, or synchronous
expression executor is introduced by this route.

LCIR has explicit `task.create`, `task.sleep`, and `task.await` control flow.
Await stores the checked live row, registers a structured one-child join,
publishes its state, and returns pending. A completion notification puts the
parent back in the ready queue; the callback takes the child's exact typed
result, reloads live values, and enters the checked continuation. Typed async
run/test harnesses create one executor for the root Task, drive it to a terminal
state, take the exact result, and destroy the executor.

Current typed coverage includes cleanup-free, non-inout coroutines with direct
scalar/refined/product/Text parameters, results, and live values, plus
closed sums whose payload graph uses those shapes. The collision-free carrier
gives managed sums one static union of exact pointer offsets, and pack leaves
inactive pointer lanes zero. This applies equally to coroutine parameters,
suspension rows, and completed Task results without changing typed-task v1. A
fallible callback creates one activation-local fault context attached to its
executor. Checked arithmetic, assertions, ordinary fallible invokes,
caller-side preconditions, and callee-side postconditions record only the first
fault on the active Task. Await propagates the child's `Faulted` or `Cancelled`
state; it never converts either state into a source `Result`. Task handles may
be live only as suspension bookkeeping.

Selected async roots with `requires`, async inout/writeback, lexical cleanup
across suspension, raw readiness, Task combinators, List/TextMap frame values,
and dynamic concepts still select the complete legacy route.

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

Checked `Task.sleep` is admitted only inside an async function. The LCIR
terminator consumes canonical `Int` milliseconds; lowering extracts the single
field from a source `Duration` before reaching that boundary. It has explicit
normal and fault edges and contributes `MAY_FAULT` plus `NEEDS_EXECUTOR`, but it
does not suspend or collect. A negative input raises `InvalidSleepDuration`.
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

The additive factory advances the native runtime ABI to component 16 and adds
`typed-timer-v1` plus `runtime-v10` to the exact identity. The typed-task and
wait wires remain version 1; the timer carries no universal value, moving-GC
root, or new scheduler protocol.

## Blocking I/O

Operations that cannot use readiness directly are submitted to a process-wide
bounded blocking pool:

- four named worker threads;
- a bounded queue of 256 jobs;
- per-executor completion delivery through the lazy worker mailbox and reactor
  completion source.

Task cancellation invalidates or cancels its outstanding registrations.
Generation checks prevent a late notification or worker completion from waking
a reused Task registration.

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
`.await`, and `Task.all` propagate a child fault. `Task.settled` and
`Task.race`, whose purpose is to observe terminal child states, represent the
same fault as `TaskFault`; this includes an artifact construction-proof replay
failure. OOM alone remains process-level and is not a Task terminal value.

Current join modes implement the language's tuple and list forms of:

- `Task.all`;
- `Task.settled`;
- `Task.any`;
- `Task.race`.

Tuple inputs preserve heterogeneous result types. List inputs support a dynamic
number of homogeneous tasks. Join-result resources are transferred to the
parent before retired children are reclaimed.

## Current limits

The current runtime is process-local and single-thread scheduled. It is not a
shared-memory work-stealing executor. Task ownership transfer across arbitrary
async call boundaries, a general user-facing cancellation handle, generators,
streams, actors, persistent coroutines, and remote execution are not
implemented language features.

Linux x86-64 and macOS arm64 run the native async/standard-library gates in CI.
A complete Windows native job is configured, including async fixture closure,
but it is not verified runtime or release evidence until a real Windows runner
result is recorded.
