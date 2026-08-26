# Async runtime

Loom async functions are stackless coroutines lowered from MIR into
compiler-generated state machines. A `Task[T]` is a one-shot asynchronous
computation, not a pull-based generator and not a wrapper around a host-language
future.

## Coroutine descriptor

Each lowered async function has a compiler-private descriptor containing:

- resume, cancel, and trace functions;
- value-slot and witness-slot counts;
- the result slot;
- state count and per-state live bitmaps.

Locals that survive a suspension are stored in the Task frame. The MIR
validator recomputes suspension liveness, and the GC traces only slots live in
the current state. Captured witnesses have separate slots and an owned proof
arena.

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

- `LoomWaitSource` for timers, file-descriptor readiness, and worker
  completions;
- `LoomRegistration` with a key and generation;
- one-shot `LoomReadyNotification` records;
- cancellation and stale-registration rejection.

The Unix implementation uses the Rust `polling` crate, which maps to the host
readiness mechanism (epoll on Linux and kqueue on macOS). Notifications carry
the suspended frame identity and enqueue the corresponding Task for another
resume step.

The public safe `WaitSet` utility duplicates registered file descriptors, so a
caller closing or reusing its descriptor cannot invalidate the registration.
Compiler-generated resource tasks also retain or duplicate the descriptor
according to the scoped resource operation's ownership contract.

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
Windows currently runs CI only for selected platform-independent compiler
layers and is not evidence for this runtime.
