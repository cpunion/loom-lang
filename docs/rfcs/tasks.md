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

The current source slice supports async functions/methods, async main/tests,
one-shot local Tasks and postfix `.await`/`.await?`. Hot creation enqueues a child
without running its body inline. One owner thread processes the ready queue;
this is cooperative execution, not parallel threads. Child faults propagate at
await, and parent failure cancels and drains queued or suspended descendants.
The [source example](../../compiler/examples/tasks) illustrates the available surface.

`std.time.monotonic_ns` reads a clock with an unspecified process-local origin,
not calendar time. `sleep_ns` and `sleep_ms` measure from the start of their task
body; `sleep_until_ns` uses an absolute deadline on that same clock. Arguments
and deadlines are evaluated once, not again on resume. Timer notifications now
requeue suspended tasks; the idle owner blocks in a reactor created lazily on
the first external wait. There is no thread per task, spin loop or fairness
promise. Past deadlines and zero delays are valid but do not guarantee a yield.
See the [timer example](../../compiler/examples/timers).

Direct Task parameters and returns now transfer one-shot obligations, including
nested `Task[Task[T]]`. Sync helpers expose their owner requirement through a
Task-bearing parameter/result and use the caller's owner; async constructors adopt
Task parameters without running their bodies. A Task-valued result stays below
its completed producer until the actual consumer extracts it, preserving subtree
cancellation even when that producer is transferred again. Already-evaluated
call arguments remain obligations until the call executes.
Consumption through an unselected `comptime if` remains unsupported: abstract
checking must establish the transfer rather than drop a live parameter's obligation.

Active `defer`/`scoped` cleanup now crosses await through frame-backed captures.
The independent `std.resource.NoSuspend` marker forbids live lexical bindings,
stored aggregate members and already-evaluated operands across await. It also
rejects async parameters, Task payloads and marker-erasing dyn conversion.
Function signatures alone do not retain their parameter/result values. Cleanup
bodies cannot suspend or create/consume Tasks. See the
[cleanup example](../../compiler/examples/async_cleanup).

Async concept/impl methods use the existing typed constructors for concrete,
generic and dynamic calls. The async modifier is part of conformance, not an
overload discriminator. Defaults, associated results and type/comptime parameters
share ordinary method specialization. Sparse witnesses call constructors; a private
label preserves the actual dynamic creation site. Synchronous dynamic methods can
also transfer direct Task parameters/results using their caller's owner.
Async MustScope/NoSuspend parameters and scoped receiver escape remain rejected;
this does not establish a structured resource lifetime across a child call.
See the [method example](../../compiler/examples/async_methods).

Named async references have type `fn(A) Task[B]`, shared with synchronous Task
factories. Copying or storing the code pointer creates no Task; invoking it retains
the direct-call owner and one-shot obligations. Async constructors receive the
actual indirect call location. A synchronous factory runs inline and creates any
children at its own body call sites. The [callback example](../../compiler/examples/task_callbacks)
covers both. Compile-time Task references and capturing closures remain unsupported.

Task-bearing tuples/records now transfer whole values or individual fields,
including nested tuple destructuring. Async parameters adopt each contained Task;
completed producers retain all returned subtrees until extraction. Metadata-only
fields remain readable after Task fields move. Copying a consumed Task field,
dropping other fields through a temporary projection, or replacing a live field
group rejects. See the [aggregate example](../../compiler/examples/task_aggregates).

Task-bearing Lists/enums,
socket adapters, general worker operations
and joins are explicitly unfinished, not removed requirements.
Synchronous I/O still blocks the owner thread. Public outcome inspection remains
future work.

`std.file.tasks` supplies byte/text read/write Tasks using the same completion
notifications. A lazily created pool caps workers at four per owner; it does not
bound queued job memory. Jobs retain native Files and copied buffers,
not GC references. Write bytes are snapshotted once per submitted operation;
completed read bytes copy back on the owner. Source code retains read/write loops,
UTF-8 policy, errors and explicit close. Cancellation removes queued inputs or
waits for a running call to finish before cleanup. Open/create and normal close
also use workers: an open result owns its File until extraction or cancellation;
close removes its owner-local token before fallible setup. Tokens are never reused.
Duplication and failure-cleanup close remain synchronous; a stuck native call can
delay drain. This is not a general source-level blocking-work executor or support
for resource-bearing Task results.

Private async intrinsics must be awaited directly: Loom lowers them to suspension
of the caller's frame, not separately queued Tasks. Public async function calls
still create hot child Tasks, including the source timer and file wrappers.

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

Async cleanup captures use authoritative frame fields throughout the resume,
not snapshots saved only at suspension. Each task retains only cleanup site IDs
and code pointers. Normal exit pops before a direct callback; cancellation pops,
reloads the moving frame and catches each callback's faults independently.
Cleanup-local variables stay ordinary native locals. Unstarted tasks register
no cleanup and acquire no body-local resources.

On a resume fault, a take-once hook first retires descendants and the current
wait, before live synchronous-helper cleanup may close borrowed handles. Native
helper cleanup then drains before stack unwind; the catcher finally drains the
current task's frame cleanup. All task/root borrows are released across generated
callbacks. Captured diagnostics contain no managed pointers; secondary cleanup
faults cannot replace the first failure. No native stack registration survives
a returned suspension.

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
for OS readiness rather than separate handwritten platform reactors. Source
timers and file-worker completions now use this wait path; readiness tests do not
imply source socket APIs or general worker execution. Public raw-fd wait
constructors and a runtime registry of join names are not required.
