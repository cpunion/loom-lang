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
| `first_ok` | First `Result.Ok`; otherwise ordinary errors in input order. Execution/cleanup faults fail the join. |

`any` and `race` cancel and drain remaining children before returning. Tuple
`all`/`settled` preserve heterogeneous element types; `any`/`race` require a
common value type. Lists support dynamically sized homogeneous task sets and
transfer the whole set's obligations. Empty `all`/`settled` lists produce empty
results; `any`/`race` require a nonempty input. `(a(), b()).await` is tuple-all
sugar, implemented through the trusted source `std.task.all` tuple overload;
ordinary names and shadowing do not select its policy.
`first_ok` accepts a List of homogeneous Result Tasks and returns
`Result[T, List[E]]`; empty input is `Err([])`. It cancels/drains remaining
children on success. It does not turn discarded completed values into external
resource finalizers; resource-producing races must retain explicit cleanup.

## Shared workers

**Accepted semantics; the source API is `std.task.worker.run(fn() T) Task[T]`.** Ordinary
data remains shared by default across explicitly requested workers. There is
no implicit graph copy, ownership/borrow syntax, or blanket rejection of
unsynchronized mutable aliases. Async alone does not request parallel execution.

Individual shared storage accesses must remain memory-safe. A read observes a
fully initialized, well-typed value, not a torn managed pointer or mismatched
enum tag/payload. Container bounds, backing-storage lifetime and GC relocation
must remain safe even during concurrent resize. A stale bounds observation can
lead to an ordinary bounds fault, never an unchecked access to freed storage.
This does not make a sequence of accesses, a function call, or an entire library
operation atomic. Memory safety is not a promise of race-free application logic.

For example, consider two workers each calling this function once, on the same
List initially containing `[0]`, with no other writers or resizing:

```loom
import std.list.get
import std.list.set

fn increment(values List[Int]) {
    set(values, 0, get(values, 0) + 1)
}
```

After both workers complete, the element may be `1` or `2`: both reads may
observe `0`. To guarantee `2`, both callers must use the same explicit lock
around the complete read/modify/write, or an explicit atomic update operation.
Locking only the `set` calls is insufficient. The source [mutex API](../../compiler/README.md#scoped-mutexes)
uses an ordinary `scoped` guard, without an ownership or borrow annotation.

Publication to a worker, successful completion/join, and synchronization
release/acquire establish visibility of preceding writes. Unrelated workers
have no implicit source order. The optimizer still needs equivalence evidence
to introduce parallelism into ordinary sequential code; explicit workers are
not permission to drop existing data, control, effect, fault or cleanup ordering.

Contracts and persistent type constraints remain guarantees. A fact about
mutable shared state cannot justify removing a check if another worker can
invalidate it before use. An invariant-preserving update must remain valid under
interference, not merely pass a check on a stale snapshot. Synchronization can
establish a protected boundary only when all relevant aliases follow it; locking
one handle does not protect against an unrestricted alias. Reject an unproved
strengthening or transformation, not ordinary unrefined sharing. See
[sharing and persistent constraints](language-foundation.md#sharing-and-persistent-constraints).

Workers retain structured Task completion, fault and cancellation semantics.
Cancellation is cooperative; join/drain cannot finish while a worker still
accesses shared values or runs cleanup. Scoped resources and owner-local native
tokens do not become transferable merely because ordinary data is shared.

The implementation closes these boundaries together:

1. Register participating mutators and roots; collection relocates a shared graph
   only while all participants are at safe points. Blocking native waits must
   not prevent that rendezvous, and cancellation/exit must retire registrations.
2. Lower shared storage reads/writes and resizing with safe publication and
   lifetime rules; provide explicit synchronization through narrow runtime
   primitives and source-library policy. Proven local accesses retain direct code.
3. Integrate actual parallel Loom execution with Task completion notifications,
   fault capture and cancellation drain. A single execution lock is not CPU
   parallelism; the native I/O pool is not this executor.

Acceptance needs native parallel shared-update and synchronized-update examples,
resize/aggregate publication under moving-GC stress, and fault/cancellation
cleanup. The lost-update outcome should be forced with synchronization in a
focused test, not required to appear by chance. Measure local scalar/List code
as well as parallel work; ordinary functions must not acquire scheduler context
or per-access locks without need.

The [worker guide](../../compiler/README.md#shared-workers) records runnable
examples and current conservative boundaries. In particular, worker-enabled
builds do not reuse sequential heap-observation certificates. Scalar snapshots
and private List construction/results have proof support through finite helper
contracts. Shared entry, old and current observations are independent; length
nonnegativity survives interference, stale equality/index guards do not. General
synchronized heap reasoning remains open. Escape analysis composes direct-call
publication, aliases and returns, including recursion; only helper parameters
whose every call context is private lose guards. Unknown, callback and dynamic
contexts remain conservative. GC roots and cancellation checkpoints remain.
Private access guards do not make source library calls atomic.

Explicit shallow copies can keep a private outer List header even when their
elements are shared mutable values. Checked allocation/return paths establish
that origin, not a `clone` name rule. Initializing private storage does not
publish it; publishing a mutable graph into shared storage or an opaque call
conservatively loses privacy. Nested elements retain their own alias evidence,
and no atomic snapshot or deep isolation is implied.

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

Source `std.task.cancel_when(task, trigger)` waits for completion or requests
cancellation from a terminal trigger, preserving the work's actual outcome after
drain. Trigger results/existing faults are deliberately discarded: it is a signal,
not a second result-bearing operation.
`std.task.deadline.at` supplies an absolute monotonic timer;
`after_ns` computes that deadline at the call, not in a queued body. A completed
result cannot be erased to manufacture timeout status. Cancelling the waiter
drains both subtrees; an earlier work fault retains priority over trigger cleanup
faults. These are cooperative source policies, not hard execution-time limits.

Direct Task parameters and returns now transfer one-shot obligations, including
nested `Task[Task[T]]`. Sync helpers expose their owner requirement through an
owned Task-bearing parameter or a Task-bearing result and use the caller's owner; async constructors adopt
Task parameters without running their bodies. A Task-valued result stays below
its completed producer until the actual consumer extracts it, preserving subtree
cancellation even when that producer is transferred again. Already-evaluated
call arguments remain obligations until the call executes.
An undetermined `comptime if` retains check-only uncertainty for visible Task
bindings. It does not prove them consumed or execute either branch. Each concrete
instance selects its branch and checks all actual transfers before emission,
including omissions, repeated reads, overwrites and control-flow joins. Required
postconditions still need abstract proof; pending Task analysis is not proof evidence.

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
Async MustScope parameters are checked borrows when their call is directly
awaited. The caller retains the owner through completion or cancellation drain;
the borrowing Task cannot be saved, forwarded, joined or returned. Checked
direct calls also retain Dispose-only borrows; indirect calls require MustScope.
NoSuspend parameters/results remain rejected. Fresh MustScope results are owned
by their completed Task until one-shot extraction; an unextracted result drains
through typed frame callbacks on fault/cancellation. Extraction transfers to a
receiver's `scoped`, including after `.await?`. Scoped/borrowed resources cannot
be returned or transferred into a Task. Tuple/List `all`/`settled`, List/pair
`any`/`race`, cancellation signals and deadlines use the same typed result
ownership. `first_ok` inspects a terminal enum case without extracting its payload,
then transfers the selected Ok or an input-ordered guarded error List. General
borrowing Tasks remain unfinished.
See the [method example](../../compiler/examples/async_methods).

Named async references have type `fn(A) Task[B]`, shared with synchronous Task
factories. Copying or storing a named function value creates no Task; invoking it retains
the direct-call owner and one-shot obligations. Async constructors receive the
actual indirect call location. A synchronous factory runs inline and creates any
children at its own body call sites. The [callback example](../../compiler/examples/task_callbacks)
covers both. Capturing closures use the same callable representation, excluding
live Task and scoped resource captures. Compile-time construction may retain an
async reference without invoking it; creating, transferring or awaiting real
Tasks still cannot execute at compile time.

Task-bearing tuples/records now transfer whole values or individual fields,
including nested tuple destructuring. Async parameters adopt each contained Task;
completed producers retain all returned subtrees until extraction. Metadata-only
fields remain readable after Task fields move. Copying a consumed Task field,
dropping other fields through a temporary projection, or replacing a live field
group rejects. See the [aggregate example](../../compiler/examples/task_aggregates).

Inline MustScope aggregates can retain resources and Task fields together. A
resource parameter borrows the Task leaves rather than consuming them; only the
owning scope can explicitly await or transfer those leaves. Task-free reads and
forwarding borrows need no Task owner and remain valid in cleanup. Borrowed
parameters do not grant authority to create other Tasks, including through
indirect calls. Cleanup still cannot create, consume or await Tasks; fault and
cancellation drain existing children before resource disposal. Resource Lists
with live Task elements and Dispose-only mixed aggregates remain conservative.
See the [mixed scope tests](../../compiler/examples/cleanup/resource_task_scope_test.loom).

Task-bearing enums transfer once and expose the active payload through ordinary
`match`, including `Option`, `Result` and `?`. Bound Task-bearing payloads must be
consumed; `_` cannot drop them. A whole wildcard may cover the remaining Task-free
variants after Task-bearing variants have explicit arms. Named whole bindings
transfer the enum and keep its type's obligations. Even a known empty value is
matched or transferred at source level; native adoption/return visits only actual
Tasks and creates none for empty branches. See the
[enum example](../../compiler/examples/task_enums).

Task-bearing Lists now transfer as one dynamic group. The source
`std.list.transfer` package supplies `append` (returning the same header) and
`take_last` (returning `Option[(T, List[T])]`). The caller consumes each extracted
element and transfers the remainder. Normal Lists retain shared mutation;
Task-bearing Lists cannot use copying get/index/push/set operations. Recursive
payloads use ordinary typed visitor functions for adoption and return marking.
Loop backedges must preserve entry obligations; all continuing exits must agree.
Literal `while true` has no zero-iteration exit, so it can consume an outer group
before breaking or returning. See the [list example](../../compiler/examples/task_lists).

Private completion observation now records children once and orders ready
notifications by actual terminal order, including registration after completion.
It keeps only IDs/indices, not managed pointers. Selecting a notification leaves
the typed Task obligation intact for ordinary await, and unrelated parent waits
do not lose notifications. Parent cancellation removes registered observations
before its cleanup. The private `std/task` tests exercise these operations through
Loom suspension lowering.

`std.task.outcome(task).await` now consumes a child and returns the source
`Outcome[T]` enum, preserving successful values, ordinary Result errors and
Task-valued result obligations. No-result Tasks match `Outcome.Completed(_)`,
without an explicit source Unit type. `TaskFault.message` contains owned
diagnostic Text, including creation locations and the test name when present.
`std.task.cancel(task)` drains descendants, OS work and cleanup before returning
that child's outcome. Already-terminal children retain their real result;
cleanup faults return Faulted. This operation is synchronous and may delay the
owner while an active blocking OS call finishes. Neither API catches OOM or
unexpected runtime failures. The private outcome schema validates exact typed
payloads; generated ordinary enum control flow constructs the source result.
See the [outcome example](../../compiler/examples/task_outcomes).

List `std.task.all/settled/any/race` now implement the policies above in Loom.
Children register once; checked indexed replacement selects their typed handles
without rescanning or a per-input wrapper Task. Private group helpers add one
Task per group. Input-order collection, dynamic/empty inputs, no-result payloads
and Task-valued results share the native generic path. `any` faults with the
first observed failure when no input succeeds; an ordinary Result error counts
as success. Losing producers are retired with their returned subtrees intact.
A cleanup fault fails an otherwise successful join, while an existing primary
fault remains authoritative. See the [join example](../../compiler/examples/task_joins).

Heterogeneous tuple `std.task.all((a(), b(), ...))` preserves distinct result
types and observes every child before suspending. It cancels and drains the
remaining children after the first fault. `std.task.settled` accepts a tuple of
any statically known arity, including zero and one, and returns the ordered
heterogeneous `Outcome` tuple. Its Tasks are already hot; it awaits every
outcome in input order without early cancellation. Tuple `.await` accepts a
tuple value from a literal, binding, or call, evaluates it once, and uses the
same `all` fault/cancellation policy without requiring a source import. It
supports empty tuples produced by a type-pack call and singleton `(task,)`;
source `()` remains invalid. Scalar Task `.await` is unchanged. Tuple elements
must all be Tasks; arbitrary Awaitable values are not supported. General worker
operations remain unfinished requirements.
Two-argument homogeneous `any`/`race` calls delegate to their List policies,
including loser cancellation and cleanup.
Synchronous I/O still blocks the owner thread.

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
delay drain. This is not a general source-level blocking-work executor. Source
resource-bearing Task results use independent typed result-cleanup callbacks,
not the native File token's special result path.

`std.net.dns.resolve` uses the same pool for OS hostname resolution, including
hosts-file entries. Native workers snapshot Text inputs and return numeric socket
addresses; the owner copies bytes through the shared completion operation. Loom
constructs the address List and owns error policy. `std.net.tcp.connect(host, port)`
interleaves families with bounded, staggered workers over a shared address queue.
Defaults are two pending attempts, a 250 ms worker delay and a 30-second total
cancellation deadline. `ConnectOptions` configures these; `connect_any` accepts
numeric endpoints without DNS. The parent records sockets before suspension,
then drains workers and closes every loser, including completed connections.
All-failed, resolution, invalid-option and deadline failures remain distinct.
The numeric single-argument connect path adds neither DNS nor a deadline.
No DNS cache is provided. Running OS resolution cannot be interrupted and may
delay cancellation drain beyond the deadline.

TCP local/peer endpoint queries return numeric Text. `set_nodelay` controls the
native TCP_NODELAY option; `shutdown_write` ends sending without revoking the
token or receive registrations. Finish intended writes before half-close, then
close the stream after child waits drain. The
[half-close example](../../compiler/examples/tcp_half_close/README.md) exercises
EOF-delimited requests and responses with the ordinary source Task machinery.

`std.net.udp` binds numeric IPv4/IPv6 endpoints and uses the same readiness leases.
Source `receive` returns one packet and its numeric sender, treats empty packets
as data, and reports consumed oversized packets as `Truncated`. `send` never
splits a packet. Copies share an owner-local socket; close rejects pending leases,
and abort revokes aliases and wakes pending operations to fail. Task deadlines
and cancellation need no UDP-specific executor. No delivery/order guarantee,
implicit DNS, connected datagrams or multicast/broadcast policy is provided.

`std.net.tls` composes TCP Tasks with a Rustls packet engine. Client construction
always verifies the server's certificate chain, validity period and DNS/IP name.
Default trust is a compiled Mozilla root set; applications can supply explicit
PEM roots. Both sides can supply an explicit PEM chain/key identity. Servers
select anonymous clients or require a certificate rooted in an explicit client
CA set; certificate validity and client-auth purpose are verified before accept
succeeds. A fresh verified peer leaf DER copy is available for application policy,
not an implicit authorization rule. Configuration
is copied into native state before transport suspension. ALPN is optional.
The engine retains no GC pointers, schedules no I/O and owns no executor.
Its archive is selected only for emitted TLS references, including cached objects.

The `Connection` API permits one reader and one writer concurrently across all
aliases; same-direction overlap returns `Busy`, not implicit queueing. Encrypted
output is serialized with generation-checked completion notifications. A reader
does not wait behind an active writer's application-data backpressure. Failed or
cancelled operations drain their transport children and retire the connection,
revoking socket registrations and waking the other direction to fail.
`shutdown_write().await`
flushes close-notify while retaining the receive side; synchronous `close` retires
the transport and fits `defer`. Unexpected TCP EOF is not a successful TLS EOF.
Deadlines use ordinary Task cancellation. Certificate
revocation and session-resumption policy remain future work, not implicit guarantees.

`std.process.tasks.capture` / `capture_input` submit copied native commands and
binary input to the same bounded pool. Their source result and configuration
match `std.process`; pipe draining, spawning and reaping share its native
implementation. Workers return owned native output, and the owner copies both
streams into rooted Bytes. Queued cancellation drops the job; running
cancellation waits for capture and reaping, with no process-tree termination.
Extra pipe threads are bounded by active capture jobs, not the pool's four-thread
limit. Output is buffered without a size limit; streaming and hard process
timeouts are not implemented.

Private async intrinsics must be awaited directly: Loom lowers them to suspension
of the caller's frame, not separately queued Tasks. Public async function calls
still create hot child Tasks, including the source timer, file and DNS wrappers.

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

The private [shared-heap ABI](../../compiler/runtime/src/shared_heap.rs) can trace
registered native mutators' roots after a cooperative rendezvous. Native waits
park without retaining heap borrows; joining the shared scope restores its heap
to the initiating thread. Private per-object access guards preserve lock identity
through movement and park contended waiters; faults release internal guards
before user cleanup. Idle owner reactors and native-I/O drain waits also park.
These boundaries do not yet enable generated Loom workers or protect existing
container operations automatically.

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
timers, sockets, file-worker completions and CPU-worker notifications use this
wait path. Public raw-fd wait
constructors and a runtime registry of join names are not required.
