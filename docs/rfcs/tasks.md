# Tasks and suspension

Status: **Accepted direction**. This consolidates the previously accepted async
decisions; implementation is incremental. See the
[implementation status](../project/implementation-status.md).

## Source behavior

An `async fn` declares its logical result type; calling it yields `Task[T]`.
An omitted result type still means no value result. Async entry points include
`async fn main` and `test async fn`.

Arguments evaluate once, left to right. The call creates a child task, links it
to its parent, and enqueues it without executing the body on the caller's stack.
Tasks are scheduled, not cold computations requiring a separate start operation.
Argument-evaluation faults belong to the caller. Preconditions execute when the
child first runs; their failure belongs to the child, with creation-site blame.
Postconditions are mandatory proofs over the logical normal result, not task
construction.

Suspension uses the postfix keyword `.await`, only inside async functions and
async tests. Chaining and `.await?` preserve ordinary expression and `Result`
semantics. Prefix await is invalid syntax. `.await()` is not a method call;
`.await!` is not a force-success operation. Synchronous helpers do not drive a
nested executor.

A Task is a one-shot structured obligation: it cannot be silently dropped,
discarded, copied, overwritten while live or awaited twice. Parameters, returns and
structured bindings may transfer that obligation, including within aggregates.
This introduces no ownership, borrow, lifetime or pin syntax.

## Completion and cleanup

The outcomes are `Completed(T)`, `Faulted(TaskFault)` and `Cancelled`.
An ordinary `Result.Err` is a completed value, not a task fault. OOM remains an
uncatchable process-level fault. There are no detached tasks in this design.

Parent failure or cancellation cancels and drains its children. Cancellation
is observed at suspension or compiler checkpoints; it is not asynchronous stack
interruption. Active lexical cleanups run synchronously, once, in LIFO order.
Cleanup cannot suspend. A `NoSuspend` resource cannot remain active across an
await; scoped values cannot escape into a child without a valid structured
lifetime. No ordering between independent siblings' cleanups is promised.

Cancellation request and terminal completion are distinct. Queued blocking work
may be removed before starting; already-running work must finish and release its
operation buffers before its task becomes terminal. A join cannot return while
cancelled children still perform external effects or cleanup.

## Source-library composition

Join policy belongs to Loom `std`, over narrow task primitives:

| Policy | Result and completion rule |
| --- | --- |
| `all` | Results in input order; a fault cancels and drains remaining children. |
| `settled` | All task outcomes in input order, without early cancellation. |
| `any` | First completed value; failure if none completes successfully. |
| `race` | First terminal outcome. |

`any` and `race` cancel and drain remaining children before returning. Tuple
`all`/`settled` preserve heterogeneous element types; `any`/`race` require a
common value type. Lists support dynamically sized homogeneous task sets and
transfer the whole set's obligations. Empty `all`/`settled` lists produce empty
results; `any`/`race` require a nonempty input. `(a(), b()).await` is tuple-all
sugar. Concrete library spellings are not compiler dispatch tables.

## Implementation boundary

The current source slice supports direct async functions, async main/tests,
one-shot local Tasks and postfix `.await`/`.await?`. Hot creation enqueues a child
without running its body inline. One owner thread processes the CPU ready queue;
this is cooperative execution, not parallel threads. Child faults propagate at
await, and parent failure cancels and drains queued or suspended descendants.
The [source example](../../compiler/examples/tasks) illustrates the available surface.

Active `defer`/`scoped` cleanup cannot yet cross an await. Task transfers through
parameters, returns and aggregates, async methods/function values, source timers,
asynchronous I/O and joins are explicitly unfinished, not removed requirements.
Synchronous I/O still blocks the owner thread. Public outcome inspection and
cleanup across suspended activations remain future work.

Loom lowers suspension into ordinary typed constructor/resume functions, using
private frame/task primitives and control flow; LLVM does not lower source await.
Ordinary functions retain direct code without a scheduler or frame.
Suspended state and results need persistent, updateable GC roots; stack root
slots and interior pointers cannot outlive their native activation.

The [frame-root ABI](../../compiler/runtime/src/frame_roots_abi.rs) reuses one
outer GC root frame for the owner's entire run/drain scope. Its dense collection
holds updateable allocation-base pointers and permits non-LIFO removal. Identity
slots may be reused only with a new generation; collection scans live entries,
not the historical slot capacity. No vector element address is registered as a
root. The root set itself remains native, owner-thread state and does not escape
the scope. This is not a general root handle valid after its owner returns.

Frame payloads reuse the existing zeroed typed GC allocation and tracer, without
changing ordinary record semantics. Allocation-crossing code reloads the frame
base before deriving field addresses. A completed result stays rooted until its
receiver has registered a typed snapshot; removing the producer's root does not
perform cleanup or collect. The Loom lowerer computes cross-await liveness and
state transitions; this storage boundary itself does not manage suspended cleanup.

The private [fault boundary](../../compiler/runtime/src/fault_abi.rs) wraps one
native resume activation inside that outer root scope. A language fault first
owns its diagnostic bytes and drains lexical cleanups while captures and root
slots are live. It then restores the boundary's root head and uses Rust
`C-unwind` through LLVM unwind-table frames to reach the catcher. Secondary
cleanup faults retain the first diagnostic. No panic hook or per-function catcher
is installed; without a boundary, a fault still terminates the process. Unknown
Rust panics, OOM and runtime corruption are not task outcomes. The runtime requires
`panic=unwind`; this is not a general foreign-exception recovery mechanism.

Captured diagnostics contain no managed pointers and must be released after
materializing an outcome. The scheduler uses this boundary to propagate task
faults; it does not preserve stack cleanup across suspended activations.

Wait registration is a private runtime boundary for absolute monotonic timers,
borrowed native readiness handles, and externally completed operations. A
registration carries a reusable slot and non-wrapping generation. It is retired
before one notification is queued. Stale or duplicate completions cannot wake
a replacement registration. Notifications carry opaque owner identities, never
GC or stack pointers, and execute no continuation inline. Cancellation removes
active waits, not notifications already queued; the scheduler must validate the
owner's generation when consuming a notification.

The native ABI in [wait_abi.rs](../../compiler/runtime/src/wait_abi.rs) uses
fixed-width C-layout records and an opaque reactor. Its version is internal,
not a published compatibility promise. A successful registration borrows its
handle until firing, successful cancellation or reactor destruction. Separate
read/write registrations may share a handle; overlapping interests reject.
The owner must stop all producers and waiters before destroying the reactor.
One owner consumes waits and notifications; producers may publish concurrently.
Completion publication remains committed if the subsequent OS wake reports an
error. None of these operations transfers a worker's buffer lifetime to GC.
Failed native interest updates restore the prior registrations; inability to
restore that borrowed-handle boundary is an unrecoverable runtime fault.

The reactor uses [polling](https://docs.rs/polling/3.11.0/polling/struct.Poller.html)
for OS readiness rather than separate handwritten platform reactors. Connecting
source task suspension to these waits remains a separate gate from CPU scheduling;
readiness tests do not imply source asynchronous I/O support. Public raw-fd wait
constructors and a runtime registry of join names are not required.
