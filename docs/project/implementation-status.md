# Implementation status

Only the native compiler under `compiler/` is maintained. Its
[guide](../../compiler/README.md) lists the tested subset and commands.
The former workspace compiler and its feature matrices have been removed.

Source `async fn`, `async fn main`, `test async fn` and postfix `.await` now
have a native implementation. Calls create hot child Tasks in one owner-thread
ready queue; they do not execute inline or create parallel threads. Loom lowers
suspension into typed constructor/resume functions, spilling only values needed
across awaits into GC-traced frames. Task handles are one-shot; `.await?` retains
ordinary Result propagation. A child fault propagates at await; parent failure
cancels and drains queued or suspended descendants. See the
[task example](../../compiler/examples/tasks) and [accepted design](../rfcs/tasks.md).
Source `std.time` now exposes a process-local monotonic clock and relative or
absolute timer Tasks. Deadlines are evaluated once; reactor notifications put
suspended tasks back in the ready queue. The reactor is created on the first
external wait, and an idle owner blocks instead of spinning or creating a thread
per task. Relative sleeps start when their task body runs, not when queued.
See the [timer example](../../compiler/examples/timers). Scheduling has no
fairness guarantee.

Direct Task parameters/returns, generic forwarding and nested Task results now
preserve one-shot obligations. Async callees adopt argument subtrees; completed
producers retain Task-valued results until extraction by the actual consumer.
Sync helpers expose a Task-bearing parameter/result and use their caller's owner.
Undetermined compile-time selections now defer affected Task states until concrete
instantiation, including aggregate fields, match scopes and loop/branch joins.
They retain a distinct check-only state, not invented transfers; concrete instances
still require exact one-shot flow, and mandatory proofs cannot use pending selections.
Async concept/impl methods now share the same constructor/resume path through
concrete, generic and dynamic calls. Implementations must match the declared async
effect. Default methods, associated results and type/comptime method parameters
reuse ordinary specialization. Dynamic calls also transfer direct Task parameters
and results through synchronous methods, without adding an owner or runtime ABI.
Witnesses retain only used constructors; child failures preserve dynamic creation
locations. Scoped receivers cannot escape into Tasks; async MustScope/NoSuspend
parameters remain rejected. See the [method example](../../compiler/examples/async_methods).
Named async references and synchronous Task factories now share structural
`fn(A) Task[B]` values. Parameters, returns, records and Lists store a callable,
not a live Task. Named references have an entry pointer and null environment.
Calls retain owner checks, one-shot transfer and the actual
indirect creation location. Only Task-returning callback signatures gain a private
label argument; synchronous factories use one adapter per referenced target.
Direct functions and private coroutine callbacks retain their ABI. Source async
closures use the same callable shape and owner checks, but cannot capture live
Tasks or scoped/NoSuspend resources. Compile-time construction can retain these
references without creating Tasks; actual Task creation, transfer or await still
rejects. See the [callback example](../../compiler/examples/task_callbacks).
Tuples and records now carry one-shot Task fields, including nested fields,
generic forwarding and callback/dynamic signatures. Whole-value reads transfer
all fields; field reads and tuple destructuring track each Task independently.
Ordinary metadata remains readable. Async constructors adopt all argument Tasks;
typed result projections mark every returned subtree before completion. The runtime
retains these in its existing child set until extraction, using a count and member
flag rather than another allocation. Cancellation drains all children before parent
cleanup. See the [aggregate example](../../compiler/examples/task_aggregates).
Enums now carry Tasks through ordinary matching, including Option/Result and `?`.
The whole enum transfers once; matching introduces obligations for its bound
payloads. Wildcards may skip only variants with no remaining Task payload.
Native typed matches adopt and retain only the active variant's Tasks, including
nested records/tuples and Task-bearing errors. Empty variants create no tasks.
Classification caches follow each checking/evaluation/lowering session, not
persistent compiler state. See the [enum example](../../compiler/examples/task_enums).
Task-bearing Lists transfer one dynamic group of obligations. Source
`std.list.transfer.append` returns the same header; `take_last` returns
`Option[(T, List[T])]`, consuming an empty group or transferring its last element
and remaining group. Ordinary Lists retain shared mutation; Task-bearing elements
cannot be copied through indexing or ordinary get/push/set. Memoized typed helper
functions visit recursive enum/List layouts without a runtime container visitor.
Unconditional loops merge actual break states and may consume the group before
breaking or returning. See the [list example](../../compiler/examples/task_lists).
Private multi-task observation now registers each child once and wakes its parent
from terminal notifications, without scanning a source container on every wait.
Ready entries preserve terminal order even when already-completed children are
registered later. Selection returns a caller-supplied index and detaches that
notification; the original typed handle still requires ordinary await. Unrelated
parent waits retain queued notifications. Parent cancellation removes observations
and waits before cleanup. Native `std/task` tests cover real Loom frames and moving
GC; these observation primitives are private, not public join APIs.
Source `std.task.outcome(task).await` now returns typed Completed/Faulted/Cancelled
data, including no-result and Task-bearing payloads. Fault messages are owned
Text; ordinary Result errors remain completed values. `cancel(task)` consumes and
drains a child before returning its real terminal outcome, preserving completed
results and reporting cleanup faults. Cancellation does not suspend the owner;
running blocking work must finish first. Generated ordinary enum construction
retains typed result extraction, GC snapshots and nested Task obligations.
See the [outcome example](../../compiler/examples/task_outcomes).
Source List `all/settled/any/race` now use those notifications and typed outcomes.
Input-order collection and first-completion selection do not scan all inputs at
each wait or create per-element wrapper Tasks. Losing subtrees drain before
return, including completed producers with returned Tasks. Primary faults win
over secondary cleanup faults; otherwise cleanup failure fails the join.
Generic instances preserve inferred zero-sized payloads, including no-result
Tasks, without admitting ordinary void bindings or source Unit syntax. Source
`std.list.transfer.replace` returns the displaced value and shared header, using
ordinary native get/set and the same compile-time semantics.
See the [join example](../../compiler/examples/task_joins).
Arbitrary-arity heterogeneous tuple `all/settled` joins and two-argument
homogeneous `any/race` joins are available. Tuple `.await` routes a typed tuple
of Tasks through the trusted source `std.task.all` policy without a source
import, including tuple bindings and call results; scalar Task `.await` is
unchanged. TLS and general worker APIs remain unfinished.

Lexical `defer` and `scoped` cleanup now survive suspension. Loom rewrites captured
locals into authoritative frame fields, including writes before an await or fault;
no suspended native stack pointer is retained. Normal exits pop and call cleanup
directly. Cancellation retires descendants and waits before parent cleanup, reloads
moving frames between callbacks, and preserves the first diagnostic while draining
remaining callbacks. `std.resource.NoSuspend` independently rejects live bindings,
stored members and pending operands across await, as well as Task-boundary transfer
or marker-erasing dyn conversion. Cleanup cannot suspend or use Tasks. See the
[suspended cleanup example](../../compiler/examples/async_cleanup).

The private wait ABI now provides one-shot timers, borrowed socket readiness and
cross-thread completion notifications through `polling`. Generation checks reject
stale completion; cancellation removes active registrations before handles may
close. Focused tests exercise actual timers and localhost sockets. The native
Task owner now has monotonic socket tokens for numeric-address listeners,
accepted streams, and connecting nonblocking streams. A readiness wait leases
the exact socket until notification consumption or cancellation; close rejects
an active lease. A loopback test covers readiness, explicit child cancellation,
close, and stale token rejection. Source `std.net.tcp` exposes numeric-address
`listen`, `accept`, `connect`, `read`, `write_bytes`, and explicit close through
owner-local tokens; see the [loopback example](../../compiler/examples/tcp_loopback/main.loom).
Connect accepts immediate success, otherwise waits for writable/error readiness
before checking socket error and peer state. Error notifications remain terminal,
even when a later OS error read returns no detail. Failure or cancellation closes
its private pending token. Reads append
to shared Bytes; writes retry partial and WouldBlock progress, but aliases can mutate
pending write data. `std.net.dns.resolve(host, port)` now returns numeric addresses
in OS order through the shared bounded I/O pool. Source `connect(host, port)`
tries them sequentially, closing failed attempts; see the
[hostname example](../../compiler/examples/hostname_connect). DNS inputs and results
are native-owned until the owner copies results into rooted Bytes; Loom handles
the result List and errors. Running OS resolution must finish before cancellation
can drain. There is no DNS cache, parallel address racing, peer address, half-close, connect timeout,
TLS, or structured OS-error detail yet. Source timer Tasks use the notification
path. `std.file.tasks` adds byte/text reads and writes using lazily created
native workers, capped at four threads per owner.
Workers own native Files and copied buffers, never managed pointers;
the owner copies completed reads into GC-rooted Bytes. Source code owns partial-I/O
loops, UTF-8 checks, errors and explicit close. Queued cancellation drops inputs;
running cancellation drains the OS call before parent cleanup. Open/create and
normal close also use workers. An unclaimed open result retains native ownership
until extraction or cancellation; close takes the private token exactly once.
Handle duplication and failure-cleanup close remain synchronous, and a stuck
native call can delay cancellation. Private async intrinsics suspend their caller
directly and cannot escape as Tasks; public async functions still create hot Tasks.
See the [file task example](../../compiler/examples/async_files).
Executable links enable native dead-section removal so an unused reactor does
not enter synchronous program artifacts. Library object exports are unchanged.

The private frame-root scope now reuses one existing GC root frame for an entire
native owner activation. A dense live set supports arbitrary removal and
generation-checked reuse; the collector updates its typed payload bases without
retaining vector element addresses. Forced-collection tests cover frames after
their creator returns, shared/cyclic contents and rooted result handoff. This
storage is now used by generated coroutine frames and their cleanup captures.

The private resume fault boundary now drains live lexical cleanups before Rust
C-unwind crosses LLVM frames with unwind tables. It restores the GC root chain
and returns owned diagnostic bytes; secondary cleanup faults preserve the first
error. Ordinary synchronous programs still terminate on faults. Native integration
tests exercise generated guards, runtime failures and subsequent execution under
forced GC. The Task scheduler uses this boundary for fault propagation and
descendant cancellation, not source-level exception recovery; unknown Rust
panics and OOM remain process-level failures.

The active compiler has a real source-to-native check/build/test/run path,
package/test isolation, concrete generic records/enums, shared lists, UTF-8
text, real file reads/writes and stdout, scalar constrained construction, and
mandatory postconditions within a bounded proof fragment. The Loom-written
frontend handles source and manifests, package loading, syntax, binding, typing,
proof, and checked program emission. Focused managed tests force collection
before every allocation. Constants and safe scalar weakening avoid
redundant checks; unknown construction returns an ordinary source `Result`.
Postfix `?` propagates errors with ordinary enum control flow. Recursive data
through `List` is supported; infinite inline layouts and growing generic
specializations reject.

Offline path dependencies now resolve through each module's direct manifest
entries, including transitive package loading and isolated root tests. The
[module example](../../compiler/examples/modules/app/main.loom) uses separate
instances of the same-named dependency without compiler or runtime special cases.
Canonical source roots are reused; distinct roots retain separate package/type
identities and importer-local visibility. Entry points, test roots and import
cycles use these identities, not display names. Exact HTTPS Git/fork dependencies
now share this traversal. `loom resolve` fetches selected sources and locks source
edges; normal check/build/test/run is offline and verifies real cached bytes and
directory membership. Unrelated selected-package lock entries survive. Git source
path dependencies stay inside their snapshot, and raw blob extraction rejects
links, submodules and colliding paths without running checkout filters/hooks.
Version normalization, authenticated sources and global fork policies remain open;
editable path dependencies are not frozen source snapshots.

The function expansion budget now applies per declaration, not to the number of
independent functions in an application. Unbounded specialization still rejects;
declaration-anchored errors use that declaration's span rather than a caller span
from another file.

The binding library now uses source `std.map` for per-package name indexes,
replacing linear name scans without adding a host-side table implementation.
Each name retains declaration-ordered overload candidates; callers receive
independent Lists. Production/test scopes and same-named dependency instances
remain separate. This does not add persistent frontend or proof caching.

Explicit Int refinement conversions also remove a destination check when the
source predicate proves its truth and arithmetic definedness. The bounded,
call-free implication proof preserves one evaluation of the input; unknown
cases retain `Result` construction. Exact call-free predicates and already true
`&&` conjuncts also discharge Int/Float refinement boundaries without IEEE
algebra. Conjuncts may regroup or reorder; `||` branches are not assumed true.
Construction also uses immutable scalar facts from preconditions, successful
assertions and lexical branches. Immutable Int bindings retain initializer
equalities, allowing later evidence about the original to validate a copy.
Bounded integer difference propagation combines bounds across distinct locals;
unknown and unsupported facts never become assumptions. Float copies retain only
established exact predicates. Standalone `if` guards retain the
surviving condition when exactly one branch can continue to the next statement.
The original guard remains while proved
constructors lower directly, preserving one input evaluation. Mutable bindings,
heap reads, general relational solving, two-live-branch joins and Float algebra
remain open; unsupported proofs keep their checks. Refinement-to-refinement
implication can expand direct acyclic scalar helpers with immutable locals,
preserving evaluated arguments, unused calculations, guarded preconditions and
checked arithmetic. Conditional helpers retain branch guards and early body returns;
Boolean results normalize into bounded call-free predicates. Helper loops, mutation and indirect calls remain unsupported
for this optional proof; they do not become proof assumptions.

Structural tuples support positional access and nested `let`/`var`
destructuring, including singleton patterns and explicit wildcard discards.
Initializers evaluate once before any new name enters scope; ordinary typed field
projections retain shared data, Task obligations and resource restrictions.
Native layout,
compile-time evaluation, reification, and public typed analysis use the same
aggregate semantics. The [tuple example](../../compiler/examples/tuples/main.loom)
checks evaluation order and shared-container results under GC stress.

Nested enum/tuple match patterns now lower to bounded typed decision trees.
Arms keep source-order inference and first-match selection; type-based coverage
rejects missing combinations and unreachable arms. Whole fallbacks reconstruct
decomposed values from live fields, preserving shared data and one-shot Tasks.
The [pattern example](../../compiler/examples/patterns/README.md) covers native,
compile-time and suspended execution. Int/Bool/Text/Float literal patterns now
share this matrix, using ordinary equality and typed branches. Bool coverage is
finite; other scalars require a fallback, including NaN for Float. Checked keys
normalize equal numeric spellings and decoded Text without rewriting source.
Named record patterns now share the tuple product decisions, with nominal checking,
source-ordered field bindings and explicit `..` omission. Typed field projections
and fallback reconstruction preserve shared data and Task obligations without new
runtime or checked-artifact operations. Boolean `pattern if condition` guards
now preserve source-order evaluation, bindings, scalar refinement facts and
false-path effects. They do not claim coverage. Task bindings transfer only on
success; unsafe consumption and delayed MustScope adoption reject. Native,
compile-time and async examples exercise the same typed branch lowering.
Compiler production sources retain flat patterns.
Record field declarations require newlines or `;`, not adjacency or commas.
Formatting expands declarations to one field per line and removes semicolons;
statement and constructor syntax are unchanged. Current compiler/std sources
use newlines, so this restriction needs no additional bootstrap checkpoint.
Raw triple-quoted Text supports closing-indent removal, normalized line endings
and longer quote delimiters. Formatting preserves literal spelling; lexer tests,
native/compile-time examples and multiline assertion fixtures share this path.
This adds no runtime representation or bootstrap checkpoint.
Record `let`/`var` patterns now share that field validation and the existing tuple
binding projections. Initializers evaluate once before any new name enters scope;
mutable names rebind locally without changing record fields. Nested bindings,
explicit omission, compile-time execution, shared aliases and one-shot Task
transfers have native evidence. Refutable or scoped binding patterns remain unsupported.

Postfix tuple expansion now feeds ordinary calls, enum payloads, tuples and List
literals. Literal operands expand before contextual checking; other tuples use
one saved value and typed projections at their original evaluation position.
Native/compile-time execution preserves sharing and one-shot Task transfer.
There is no runtime argument pack or expansion opcode. A saved tuple's later
field can enter a comptime position when an earlier field retains the one
runtime evaluation and the static field independently passes compile-time
evaluation. Static-first, runtime-bound, and captured-function sources remain
unsupported.

Top-level variadic functions now elaborate a final type/value pack into ordinary
generic and native parameters for each selected arity. Elementwise type patterns,
bounds, contextual function references and inferred empty tuples share the typed
pipeline, including compile-time graphs and Task transfer. Selected bodies are
checked with abstract element types before concrete specialization; unselected
arities have not been verified. Variadic postconditions reject until the checker
can prove every arity, including for uncalled declarations. Empty tuple operands
retain their evaluation at the source gap, even with no resulting arguments,
indirect/method calls or suspension. Their effects cannot enter erased comptime
arguments; those boundaries still require a prior binding. A body-level
`comptime for item in values` form unrolls an immutable tuple or visible record
binding, including the final value pack for each selected arity, and checks every
element with ordinary effects and Task rules. Records use declaration order;
generic bounds, refinement reads, lexical cleanup and shared aliases retain
their usual checks, including through nested expansion and captured callbacks.
The [record example](../../compiler/examples/pack_iteration/records.loom)
uses only typed field projections, with no runtime reflection or boxing.
An optional `key, item` binding supplies static record field names or tuple/pack
positions, including to `comptime` parameters and captured callbacks. The
[keyed example](../../compiler/examples/pack_iteration/keys.loom) exercises
value and type packs, nested maps and source-name shadowing.
A structural `(Pattern[Ts]...)` parameter takes one runtime tuple and
infers arity from its known type. `comptime map` produces an ordered typed tuple
of lexical results from an immutable fixed-shape tuple or record binding,
including the final value pack; `comptime for` remains a no-result statement.
In a selected variadic body, the same forms may iterate its declared type pack:
`comptime for T in Ts` and `comptime map T in Ts` bind each `T` to an existing
abstract element type. This is static expansion, not runtime type values or
general reflection. A lexical value tuple named `Ts` shadows that source.
Structural tuple parameters allow fixed fields around one expanded pattern,
including zero-element packs. Calls and contextual function references subtract
the fixed fields to infer pack arity; value iteration covers the entire tuple.
Several structural parameters may share one pack at any parameter position,
including alongside a final direct value pack. Each has its own fixed fields;
all expanded element types and arities must agree. Calls and function references
reuse ordinary generic checking. Arity comes from explicit types, a final direct
value pack, or the first structural tuple. Context-dependent tuples need explicit
types or a prior typed binding. Native O0/O2 and forced-GC coverage includes
evaluation order, shared aliases, closures and independent Task consumption in
the [shared pack example](../../compiler/examples/variadics/shared.loom).
Final `comptime values Ts...` packs specialize each supported static element.
Projections, expansion, static iteration and closure capture retain static
identity; captured callbacks forward current shared environments. Compiler-only
aliases introduce no runtime pack or static-value parameter slots. Immutable
aggregate elements and structural `comptime values (Ts...)` parameters are also
supported. Static tuple/record iteration retains field identities through nested
maps, lexical shadowing and closures; ordinary `let` copies remain runtime values.
See the [static pack example](../../compiler/examples/variadics/static.loom) and
[aggregate example](../../compiler/examples/comptime_parameters/aggregates.loom).
Multiple packs, methods/data packs and general pack reflection remain open. See the [variadic example](../../compiler/examples/variadics/main.loom)
and [pack iteration example](../../compiler/examples/pack_iteration/main.loom).

Named function values have structural signatures, contextual overload/generic
selection, and native calls through an entry/environment pair. Parameters, returned
callees and aggregate storage share the ordinary ABI and GC rules. Pure
compile-time invocation and returned-reference reification use the same checked
model. The [callback example](../../compiler/examples/callbacks/main.loom) runs
under forced collection; O0 scalar callbacks have no Loom runtime dependency
and retain only referenced targets. The backend now supports captured managed
environments through ordinary typed payload allocation, loads and stores.
Native O0/O2 tests cover escaped shared state, records/enums/Lists and callee
snapshots across allocating argument reassignment under forced moving GC.
Closed-build indirect allocation analysis keeps scalar callbacks root-free;
library callbacks remain conservative. Source anonymous functions now capture
only referenced enclosing bindings. Immutable bindings retain value semantics;
mutable bindings share one typed managed cell with their enclosing scope.
Nested closures, async literals and pure compile-time execution/reification use
the same environments, without new runtime operations. Scoped, MustScope,
NoSuspend and live Task captures reject before flow analysis. See the
[closure example](../../compiler/examples/closures/README.md).

Source `std.list.map/filter/fold` use those function values without new runtime
operations. They traverse the initial index range in order, preserve shared
element semantics, and execute at compile time with pure callbacks. A pinned
function-capable source checkpoint precedes this standard-library adoption.
Source Option/Result transformations use the same callbacks for value mapping,
fallible chaining and conditional fallback; Result also maps errors. Only the
selected callback is invoked, and payload sharing is preserved. Colocated source
tests exercise native, compile-time and forced-GC execution.

`std.list.sorted` provides source-written stable merge sorting with an initial
snapshot and shared elements. It works with pure compile-time and allocating
native comparators. `std.fs.entries` reuses it for deterministic byte ordering;
comparator ordering laws remain a caller obligation, not a completed proof gate.

Source `std.list.nonempty.NonEmpty[T]` keeps a nonempty shared List behind
private state. Its `copy_from` explicitly copies an ordinary List's outer header
before checking nonemptiness; wrapper aliases still share permitted updates.
This is a library invariant-preserving API, not a language-level constrained
List conversion or a fixed-shape view into the original List.

`std.list.view.View[T]` now provides fixed-shape shared ranges over ordinary
Lists. Source writes and overlapping views share element identities; removal
retains the old element, while a later append creates a distinct one. View
length remains immutable across source resize, moving GC and suspension, so a
length refinement uses the existing persistent-invariant check without copying
the source. Loom owns the view API; one private registration operation and a
watched-removal slow path maintain retained cells. Internal weak registrations
do not keep abandoned views alive. Ordinary List reads/writes/growth keep their
direct layout. Compile-time evaluation and graph reification preserve these
relationships; Task/MustScope payloads reject. See the
[shared view example](../../compiler/examples/shared_views). This closes fixed
shape, not general content-invariant alias/effect analysis.

Source `std.map` and `std.set` now use shared Lists and ordinary bounded generics
for open-addressed hash tables. Explicit `std.equal.Equal`/`std.hash.Hash`
implementations cover Int, Bool and Text; custom managed keys use the same direct
method calls. Replacement, collisions, deletion/rehash, alias-preserving growth
and clear, snapshots, and compile-time construction have source tests. The
[collection example](../../compiler/examples/collections/main.loom) also runs
under forced moving collection. Private nominal state hides mutable table
storage without new syntax or runtime operations. Key equivalence/hash laws and
stability remain caller obligations; adversarial collision protection, concurrent
maps and iterator APIs are not implemented.

Source `std.json` parses and writes in-memory JSON without runtime support.
Numbers retain their exact lexical spelling, strings decode Unicode surrogate
pairs, objects preserve field order and reject duplicate decoded keys. Nesting
is capped at 64 levels, including cyclic values supplied to the writer. This is
not yet a streaming API. `encode[T]` now specializes source code for primitive
values, Lists, tuples, visible records and supported refinements, reusing the
writer's escaping and limits without constructing an intermediate `Value` tree.
Foreign private types remain opaque; unsupported kinds report `UnsupportedType`.
`decode[T]` parses a `Value` tree, then generates typed construction for primitives,
Lists, tuples and visible records, including recursive trees. It enforces exact
field sets and Int spellings/ranges; it does not round Int through Float.
Refinements remain explicit checked boundaries: decode a wire representation
before constructing a constrained type. The
[typed JSON example](../../compiler/examples/json_encoding/main.loom)
runs at O0/O2 under forced GC. Streaming and custom wire mappings remain open.

Static concepts use explicit nominal `impl` declarations, generic bounds and
ordinary direct-call specialization. Conditional `T implements C` tests select
only that instance's branch; they do not add a public generic requirement.
Method overloads require determined selection, with concept qualification for
ambiguity. Source `std.display` provides the first shared capability. Unused
implementations stay outside native reachability. Native `dyn C` boxes only with
statically established evidence and supports generic bounds and managed aggregate
storage. Its sparse method tables retain only reachable slots for closed builds;
library exports retain their callable tables. Receiver snapshots remain alive
across allocating arguments under forced collection. Static associated types
normalize method signatures, generic results and bounded record/enum fields into
ordinary concrete types, including compile-time results. Constraint evidence is
checked at each type use rather than stored as permission on an interned type.
Concept default bodies share this checking and specialization; explicit
implementations may override them. Defaults can call required methods and use
associated types, and dynamic defaults retain the same sparse-table reachability.
Generic implementations infer header parameters from receiver types, recursively
check declared prerequisites and substitute associated bindings. They reuse
ordinary method instances and dynamic witnesses; structurally overlapping
implementations reject rather than selecting by order. Explicit dynamic associated
bindings participate in type/interface identity, normalize generic projections
and check boxing exactly. They use the existing native witness ABI;
an explicit `impl D for dyn C` can now adapt an erased value to another concept,
including associated bindings, without hidden-type discovery. It boxes the
known `dyn C` payload; implicit inclusion and zero-allocation upcasts remain
unsupported.

Associated member bounds and default bindings extend that same model. Explicit
bindings override defaults; defaults keep their declaration's name scope and
normalize against the effective implementation map. Generic callers and default
methods use the declared member promises, but implementation establishment cannot
assume an unproved promise. Required dyn bindings remain explicit, exact and
bound-checked.

Static associated families extend this to `type Item[T]`, projected as `S.Item[T]`
or `S.Concept.Item[T]`. Generic implementation arguments and member arguments
remain distinct; normalization yields ordinary concrete types. Defaults can use
member parameters, and implementations inherit their declared requirements even
when renaming them. Arity, stronger implementation requirements, unproved result
bounds and cyclic or growing expansion reject during abstract validation.
The [family example](../../compiler/examples/associated/generic.loom) covers this
without adopting the new syntax in the compiler's production sources. Dynamic
family bindings remain unsupported; ordinary exact dyn bindings keep the existing
witness ABI.

Concept and implementation methods also have independent generic parameters,
with explicit or inferred call arguments, inherited requirements and default
bodies. Calls resolve against the concept's declared domain before selecting an
implementation. Dynamic calls materialize only concrete method instances in the
selected build, retaining the existing typed witness ABI and reachability model.
No runtime type discovery, generic code generation or new bootstrap checkpoint
is required. Native objects remain build-specific, not open-ended generic libraries.

Methods accept scalar, immutable aggregate/refinement and function `comptime` parameters, with matching
positions in the concept and implementation. Static and dynamic calls share
selected-branch validation under declared bounds; known implementations in unused
bodies cannot hide an invalid selected branch. Overrides do not instantiate the
default body they replace. Dynamic slots distinguish static
values and omit those arguments from their native signatures. Default forwarding,
generic methods and static-method compile-time execution use the same checked
model. Concept-declared contracts are inherited by defaults and implementations;
each implementation must prove its postconditions.

Pure compile-time dynamic construction/calls now use those same checked witnesses,
including associated, generic, default and static method instances. Purity checking
follows all possible targets of a called interface slot, retaining the ban on I/O
even in an unexecuted runtime branch. Reified dyn results preserve admitted evidence,
receiver values and shared/cyclic containers while rebuilding witnesses in the
output queue; compile-time-only methods do not become runtime roots. The
[dynamic computation example](../../compiler/examples/comptime_dynamic/main.loom)
also runs natively under forced moving collection. Runtime capture remains
unsupported. A required proof may use the declared scalar postcondition of a
synchronous dyn concept call without learning the concrete receiver type.

Source `std.int.parse` handles signed decimal input and range errors without
runtime parsing helpers; the same function can execute at compile time. The
[arguments example](../../compiler/examples/arguments/main.loom) combines it
with constrained construction, command-line input, and ordinary source tests.

`Int` and `Float` constraints may call pure helpers with loops, recursion, and fresh data.
Their supported operation/call closure is validated even for unused constrained
declarations. Known true/false predicates remove the check or reject; unknown
inputs or unsuccessful optional evaluation retain normal runtime construction
and fault behavior. Function contracts now reuse direct acyclic helpers over scalars and inline records/tuples
with immutable locals, `if/else`, and tail expressions or early body returns. A private checked closure
preserves helper preconditions, eager/unused arithmetic and short-circuit guards;
successful entry checks provide facts, while exit checks must be proved. Helpers
used only by postconditions stay out of native reachability. The
[contract example](../../compiler/examples/contracts/README.md) exercises this
through the CLI and compile-time execution. Generic declarations still require
abstract proofs. Direct scalar and inline aggregate calls in a body requiring proof compose verified
callee postconditions, or expand a finite pure body without a summary. Synchronous
dyn calls use only the exact concept method's declared scalar contract.
Argument snapshots preserve eager evaluation; proof-only temporaries do not
change emitted calls or locals. Conditional scalar results reuse existing typed
branches for body proofs and bounded Boolean normalization for contracts. Reversed
linear relations and excluded integer interval endpoints retain branch facts.
Nested aggregate summaries retain exact proved field equalities and independent
unknowns for other fields or calls. Shared siblings remain opaque; no field
content or alias stability is inferred from their presence. Record updates and
tuple projections reuse the same proof path, including eager evaluation of unused
initializer fields. See the
[aggregate contract example](../../compiler/examples/aggregate_contracts/main.loom).
Required postconditions also reuse the bounded integer difference propagation
used by refinement construction. It derives bounds across actual symbolic values,
including inline fields and fresh callee results, not local-slot numbers.
Postcondition short-circuit guards participate in both truth and definedness
proofs. Propagation scans at most 256 facts and 32 numeric identities per boundary;
it is interval propagation, not a complete relational solver. Original entry
checks and body faults remain; see the
[relational contract example](../../compiler/examples/relational_contracts).
After direct facts fail, bounded difference-graph queries now prove relative
chains and equality cycles without absolute anchors. Negative cycles establish
inconsistent premises using mathematical arithmetic, not machine overflow.
This reuses the 32-identity/256-fact limits and adds no runtime checks or general
nonlinear solver; postcondition arithmetic still needs its own definedness proof.
Recursive proof dependencies, helper loops/mutation, returns inside helper operands,
indirect calls and dyn calls without a usable contract remain unsupported; required proofs never
fall back to runtime checks or sampled evaluation.
Required contracts also follow scalar fields through nested inline records/tuples and
Int-backed refinements. Supported input-type invariants now become facts over
those exact immutable values, without repeating construction checks or adding
entry clauses. This includes record refinements and nested refined leaves.
Bounded acyclic invariant helpers now expand on demand in a private checked
closure; their preconditions and arithmetic bounds retain execution guards.
Supported helper conjuncts now retain their evidence even when another conjunct
is outside the prover's fragment; disjunctions are not split. Helper loops/recursion
and shared contents remain outside this slice. See the
[input invariant example](../../compiler/examples/invariant_contracts).
In `ensures`, `old(expr)` currently denotes an immutable
parameter scalar or inline record/tuple scalar path; shared-data snapshots, index and
call expressions reject rather than treating a mutable alias as entry state.
Record-backed refinements now check the predicate's observed fields, rather than
rejecting every record containing shared storage. Immutable scalar/Text/record
paths may coexist with unobserved shared List/Bytes siblings. Existing aliases
retain normal sharing and mutation without invalidating the immutable predicate.
Unknown construction checks once; copies and base-record widening do not recheck.
Direct predicate helpers may copy and forward the whole record. Input-origin
summaries distinguish shared inputs from fresh local scratch, including mixed
helper arguments, recursive calls, returned aliases and preconditions. Monotone
local origins cover branch/loop assignments and match payloads. Field paths keep
input and fresh scratch separate through mixed records, tuples, enum payloads,
record updates and helper returns. Budget exhaustion widens to unknown input
reachability, never freshness. Named callbacks and locally created closures propagate these origins through
every reachable target of their checked function shape, including returned
aliases and preconditions. Captured environments retain field paths through
returns, nested callbacks and inline aggregate storage; closed references can be
predicate arguments. Input-supplied callbacks remain opaque. Reads/writes of input storage,
opaque indirect shared-input calls and mutations storing input aliases reject;
fresh scratch and immutable enum tags/payloads are allowed. Scoped/Task
payloads and unchecked replacement still reject. This is separate from the bounded proof
fragment; recursive predicates do not provide automatic proof evidence.
Native O0/O2, compile-time and moving-GC tests exercise
the [shared-field example](../../compiler/examples/record_refinement/shared.loom).
Closed inline record literals with a prover-supported `Int`/`Bool` predicate
produce the refined type directly. Closed record literals with `Float`, Text,
tuple and enum fields, and Lists of those immutable aggregates, do the same when
compile-time evaluation establishes the predicate; false constants are diagnostics.
Unknown inputs and unevaluated expressions retain the `Result` boundary.
List-backed constraints now admit length/content predicates over immutable
scalar/Text/inline aggregate elements. Fresh literals and proved pure,
non-publishing factories establish the alias boundary; explicit source `clone`
works without a library-name shortcut. Existing writable aliases cannot be
strengthened. Copies keep sharing, but raw weakening is limited to non-escaping
operations and fresh copies. Predicate input effects distinguish length from
element observations: length-only constraints permit indexed replacement and
source helpers such as `set`/`reverse`, without a runtime recheck. Appends require
the existing bounded prover to establish `predicate(n) => predicate(n + 1)`,
including helper requirements and arithmetic definedness. Nonempty/lower-bound
constraints therefore allow source push/append helpers; fixed lengths and upper
bounds do not. Native capacity growth keeps even zero-sized elements within the
source Int length range and faults before mutation. Unproved length changes,
element writes under content predicates, alias returns/publication and unknown
effects reject, including in unused concrete functions and CTFE. Closed proved
literals return the refined type; dynamic inputs retain Result construction.
No monitor, implicit copy or runtime wrapper is introduced. Native O0/O2, moving
GC and Task handoff exercise the [List example](../../compiler/examples/record_refinement/lists.loom).
Editing a predicate revokes old write permissions, including after persistent
definition-cache restoration. General predicate-preserving writes beyond these
disjoint effects and bounded length proofs, and strengthening existing mutable
alias graphs, remain open.

Record updates now accept one final `..base`, preserving missing fields from
the same nominal record declaration. Explicit fields and the base evaluate once
in written order. New values preserve shared fields without mutating the base;
generic arguments may change when retained fields remain compatible. Refinement
truth is not inherited: normal constrained construction validates the new whole
value, so failed validation cannot leave the original partially updated.
Required contracts and compile-time execution reuse the same typed operations.
Task-bearing bases transfer whole values, NoSuspend operands retain their
pending obligations, and MustScope updates reject. Native O0/O2 forced-GC tests
cover the [runnable example](../../compiler/examples/record_updates/main.loom);
scalar updates need no runtime calls at O0. This is explicit new-value
construction, not fine-grained shared-state invariant analysis.

Managed memory now uses stop-the-world copying collection with a traced
large-object space; stress mode relocates all sizes. Rewritable typed
roots cover records, active/nested enum payloads, dyn boxes, shared headers and
backing buffers. Runtime copies and generated allocation-crossing snapshots
reload relocated references; earlier arguments remain independent of later
reassignments. Static Text is unchanged. This does not add ownership syntax,
finalizers or a per-access barrier. Locals still have conservative root lifetimes;
generational/concurrent collection and complete resource cleanup remain open.

Ordinary `Float` values use IEEE binary64 arithmetic and native aggregate
layouts, without implicit Int conversion. Source `std.float` owns decimal
grammar, parsing errors and checked integer conversion; tiny runtime codecs
provide correctly rounded decimal conversion. The
[Float example](../../compiler/examples/floats/main.loom) covers generic and
managed aggregates. The same bounded evaluator now executes Float operations and
reconstructs Float/refined results, retaining IEEE edge cases. Constant Money
construction folds its predicate; dynamic inputs check once and return Result.
Money weakens only to its declared Float base. Unsupported required Float proofs
still reject rather than applying integer algebra.

Int bitwise operations use direct native instructions and the bounded evaluator.
Shift counts outside 0–63 fault; left shifts discard high bits and right shifts
sign-extend. This supplies integer mechanisms for source binary/hash libraries,
not those libraries themselves. Symbolic bitwise postconditions still reject.

Shared Bytes now support checked direct native get/set and compile-time access.
Source `std.file.read_bytes/write_bytes` preserve NUL and non-UTF-8 contents,
handle chunked reads/partial writes and close explicitly. Text reading reuses
the binary loop and then validates UTF-8. A pinned source checkpoint precedes
the new intrinsic declarations; no frozen frontend or runtime policy layer is
added. Source `std.bytes.decode_utf8` now exposes that strict validation as
`Result[Text, Utf8Error]`; successful Text is isolated from later Bytes mutation.
`std.file.read_text` reuses it and maps errors, without a second private decoding
path. These file writes truncate their destination, not atomically publish it.
Source `std.io` also exposes binary stdin/stdout/stderr without new runtime APIs.
Whole-input reads buffer to EOF; `read_chunk(buffer, limit)` appends a bounded
chunk without waiting for EOF. Writes handle partial counts without closing
standard streams. The resident editor uses chunked stdin through this source API.
Native O0/O2 and forced-GC tests cover empty/multichunk inputs, all byte values,
continued stream access, and strict text decoding. See the
[binary filter](../../compiler/examples/binary_streams/main.loom).

Source `std.fs` now exposes exclusive directory creation, native rename/replacement,
nonrecursive removal and no-follow entry classification. Native rename never
falls back to delete/copy. Missing paths and creation collisions are distinct
source errors; links and unknown Windows reparse points are not traversable
directories. This supplies namespace primitives, not a transactional cache,
hostile-path sandbox or durable publication protocol.

Source `std.hash.sha256` now supplies one-shot binary digests and lowercase hex
encoding using ordinary Int bitwise operations and Bytes. Fixed scratch storage
and virtual padding avoid copying the input. Known vectors, compile-time results
and native execution under forced collection agree. The source resolver now uses
it for locked snapshot contents and membership, not native object caching.

Source `std.process.capture` exposes concurrent binary stdout/stderr capture
with stdin EOF, literal arguments and preserved output for nonzero or signal
termination. Worker threads touch only OS pipes and Rust buffers; output copies
back into precisely rooted Loom Bytes after the child is reaped. Failure cleanup
handles the direct child, not a process tree. A support checkpoint precedes the
public intrinsic declaration. This is synchronous tooling, not an async executor
or streaming process API.
Its configuration overload now supplies child-local cwd and ordered environment
edits, optionally starting from an empty environment. The old private capture
operation is removed; both public forms share one native boundary. Source
`std.env.get` distinguishes absent, empty and non-UTF-8 values without logging
contents or mutating the process environment. Binary `capture_input` adds a copied
stdin buffer and a concurrent writer to the same capture implementation; early
stdin closure retains child output/status. Native-default SIGPIPE handling is
pipe-local on macOS and writer-thread-local on Linux, not a global policy change.
Text `run_input` uses the same protected writer but retains its stricter delivery
contract: incomplete input returns `Failed` after reaping the child, with inherited
stdout/stderr unchanged.
The source resolver configures these mechanisms for public HTTPS Git, while
private authentication remains open. None admits external state into compile-time
evaluation.

The native toolchain uses Rust 1.88 and LLVM 22. macOS, Linux, and Windows pass
the [full bootstrap and native gate](https://github.com/cpunion/loom-lang/actions/runs/34022948294).
Compiler, native integration,
and runtime tests cover real check/build/test/run, required-proof rejection,
input-boundary failures, and allocation-free scalar/record paths.
The [Loom-written compiler](../../compiler/loom/README.md) now compiles its own
sources: an existing compiler (stage 0) produces stage 1, stage 1 produces
stage 2, and stage 2 produces stage 3. These are build generations, not language
versions. Stage 2 and stage 3 executables are byte-identical on the
validated builds on each host. Stage 3 passes compiler, `std`, and example
package tests and runs the data example. Stages 2 and 3 agree on selected
type/proof failure diagnostics. This gate combines the
[bootstrap script](../../scripts/bootstrap.sh) and
[frontend integration test](../../compiler/tests/frontend.rs). The source
checker also checks its complete package closure, selected `std` packages,
and the scalar/data examples.
Linux CI independently passes the full historical-seed bootstrap and workspace
test gate; it does not rely on a compiler exported from the macOS job.

The [local staging gate](../../compiler/README.md#relocatable-local-toolchain)
builds a generic-CPU frontend and relocates it with source std and the existing
native bridge/runtime. CLI and editor commands share executable-relative tool
discovery; canonical path aliases no longer lose read-only editor queries.
The out-of-checkout file-tool trial exercises real native commands, isolated
tests and moving GC, with source queries also tested while the backend is absent.
The workflow includes this gate on all three hosts and retains a verified local
archive for each, with installation notes, dependency notices, a SHA-256 checksum,
and an extracted build/run smoke test. This is not a self-contained or formally
published release: host LLVM/linker dependencies and SDKs remain external, and
bare-command discovery still requires explicit tool paths.

The Windows x64/MSVC implementation now includes native linking, binary file
I/O with Unicode paths/arguments, drive/UNC/verbatim package paths, and native
artifact suffixes. Its [CI job](../../.github/workflows/ci.yml) passes native
bootstrap and all workspace tests, including compile-time recursion budget
errors, GC, and redirected I/O. A cold Windows checkout verifies and decompresses
the committed checked stage 0, builds a native Windows compiler with the current
bridge, then runs the regular stage 1/2/3 gate. Both Unix CI hosts reproduce
the checked bytes from an immutable source pin through `emit-checked`, including
normal source types and required proofs. No cross-platform artifact transfer or
second language frontend is needed.

The source frontend sends a checked artifact to one retained Rust LLVM/platform
tool; that tool does not parse or type-check Loom source again. The Rust seed
frontend is no longer active source. A pinned historical commit can build a
cached stage 0, followed by immutable Loom source checkpoints, when no existing
Loom compiler is supplied; this fallback is
not a compatibility commitment or a second frontend to extend. Rust remains
for the LLVM/platform boundary and runtime, not a permanent basic language.
The compiler and independent user packages now share the public
[`std.loom.source`, `lexer`, `ast`, and `parser` libraries](../../compiler/loom/README.md#public-syntax-libraries).
The standalone syntax example passes native check/build/test/run and inspects
in-memory declarations, byte spans, and diagnostics without compiler imports.
The opt-in [manifest, project, and binding libraries](../../compiler/loom/README.md#public-project-and-binding-libraries)
also share the compiler's implementation. They load selected import closures,
validate declarations/imports, and expose symbols and visible name candidates.
An ordinary project inspector accepts a package path and `std` directory,
reports root declarations and source locations, and handles load/binding errors
without a compiler subprocess. Binding is not type checking or final overload
selection; its indices belong to that program only. The additional
[typed analysis library](../../compiler/loom/README.md#public-typed-analysis)
shares the checker/prover, returning inferred expression types and concrete call
targets. Its ordinary semantic example uses only in-memory source and detects
snapshot changes. Queries cover concrete instances; snapshot comparison does
not monitor files or provide incremental reuse.

Programming tools now include [`loom fmt`](../../compiler/loom/README.md#formatting-and-editor-feedback)
and the reusable `std.loom.format.format` API. Formatting preserves parsed
structure, comments, and literal spelling; native tests and real-source
roundtrips check parseability and idempotence. `--check` is read-only,
`--recursive` visits nested source directories, and `--stdin` serves editor
buffers. The [VS Code development extension](../../editors/vscode/README.md)
uses the same compiler for unsaved-buffer diagnostics, formatting, type hovers
and definition navigation, not a
parallel language implementation. Real LSP transport and macOS VS Code
extension-host tests cover unsaved edits, cleared errors, applied formatting,
checked types and cross-file targets. `std.loom.analysis.inspect_at` shares the
token-aware queries with ordinary Loom consumers. On package errors,
`inspect_independent` can check an ordinary function and its dependency closure
in isolation, retaining hover/navigation alongside diagnostics. Global declaration
and template errors can still block this fallback; failed functions produce no
partial facts. Normal builds stay strict; dynamic calls never guess an implementation.
Name completion now uses `std.loom.analysis.complete_names` over the same package
bindings and source scopes. It preserves local shadowing, overload signatures,
test isolation and UTF-8 replacement spans, including when bodies have type
errors. `complete_at` adds receiver-type member hints: visible record fields,
tuple indices, explicitly admitted concept methods and async Task `.await`.
It shares parameter/match binding and preceding-statement checking, including
determined compile-time branches, but supplies no body/callee proof evidence.
Unknown receivers, earlier typing errors and unsupported compile-time contexts
produce no result. Checked references use exact definition spans in the loaded
closure. Conservative local `let`/`var` and unique package-private function rename
refuse unchecked occurrences and collisions; the latter includes test files and
rechecks virtual cross-file edits. See the
[cross-file rename fixture](../../editors/vscode/test/fixtures/rename_project/helper.loom).
The editor now keeps one successful typed analysis per resident package worker.
Diagnostics, hover, references and rename reuse it only after reloading the
source closure and comparing source/AST, imports, test mode, build-input bytes,
options and observed target properties. Missing or changed input files trigger a
fresh check without relying on client notifications. Failed checks and completion
repairs do not become cached evidence. Cancellation restarts the affected worker;
shutdown releases it. Real protocol tests exercise reuse, unsaved type changes,
file membership and build-input invalidation. After an edit, the worker also
reuses successful abstract checks of unchanged ordinary function definitions.
Exact function syntax is matched by package and owner, independently of symbol
IDs, declaration order and function-only file membership. Function additions,
removals, overload changes and edited bodies invalidate transitive callers.
Imports, nominal headers, trust, test and package identities still bind reuse.
Changed invariant
helpers invalidate proof consumers. Staging, closures, async/resource operations
and variadics conservatively recheck. Ordinary concrete bodies also reuse their
checked result when source text is unchanged, including scalar, aggregate, List
and generic instances. Fresh bindings drive type, call-target and source-span
remapping; assertion diagnostics follow moved files. Changed overloads and
elaboration-introduced calls invalidate affected bodies. Unsupported forms still
recheck, and all cached IR is detached from mutable public results. The same
in-memory facility is available as `std.loom.checking.check_project_cached`;
failed checks never replace its private evidence. The CLI can persist the same
ordinary definition/body evidence through its trusted-local frontend cache;
public/API rename remains open.
Qualified paths now enumerate existing package/import spellings, preserving
overloads and source-instance/test identity. Local receiver bindings take priority;
type and dyn positions filter declarations without claiming valid instantiation.
Import-statement completion now discovers direct module edges, directory segments
and public production declarations, including unsaved overlays. It reuses offline
resolution and real cached-source verification without loading incomplete imports,
fetching dependencies or writing locks. A qualified-path Quick Fix can insert an
import when one public declaration resolves through the direct offline module
identity and the revised in-memory package checks. Missing bare names receive
the same fix only for one exact public export in the selected direct closure; see the
[import fixture](../../editors/vscode/test/fixtures/import_project/library/defs.loom).
Completion-only source recovery can insert one cursor placeholder and close
unmatched EOF delimiters. It never modifies source files or supplies executable
or proof evidence; other syntax errors still reject and normal diagnostics remain.

[Compile-time execution](../../compiler/README.md#compile-time-execution) uses a
bounded Loom evaluator over the native compiler's checked model. Explicit
blocks support pure calls, local mutation, loops/recursion, and scalar or
record/enum results, including shared lists/bytes. Every runtime evaluation
constructs a fresh graph with its internal aliases and cycles preserved; no
hidden mutable global is introduced. Successful pure results can be reused
within one check, not across builds. `comptime if` selects code using a computed
Boolean or type equality/inequality, including nested generic types. Its `!`,
`&&`, and `||` conditions compose type/concept queries and pure Bool computations
with left-to-right compile-time short-circuiting. Only the actual path supplies
concept evidence to subsequent operands and the selected body; unresolved
choices defer without bypassing required proofs. This is not general type-valued
computation.
Runtime captures, external effects, faults, and exhausted
budgets reject. Scalar constraint folding shares this evaluator; required
postconditions still use the prover, with no evaluation-as-proof fallback.

Source `std.reflect.describe[T]()` now exposes a finite, visibility-filtered
type graph, including recursive fields/variants, generic arguments, function
signatures and dynamic associated bindings. Inferred generic types and
compile-time selection use the same checker; unknown types defer without
supplying proof evidence. Descriptors lower to ordinary source aggregates,
not a runtime registry, and do not retain unused methods. Native O0/O2 tests
exercise fresh/shared descriptor Lists under forced GC; compile-time-only
selection emits no runtime allocation. See the
[reflection example](../../compiler/examples/reflection/main.loom).
Descriptor IDs are local to one graph, not persistent declaration identities
or first-class source types. `comptime for/map` can traverse the field types of
a named record/tuple type, including associated type references. Source
`std.reflect.from_fields` reconstructs visible records from checked field tuples,
lowering to ordinary projections and construction. Input evaluation, sharing,
Task transfer, visibility and resource checks retain their normal rules; refined
targets cannot bypass constraint checks. The
[generation example](../../compiler/examples/reflection/generation.loom) exercises
native and compile-time generation and separately specialized closures.

Typed expression macros now invoke ordinary pure Loom generators with inferred,
definition-site-visible input schemas. Returned Text or public AST data is
validated and checked as one expression; hygienic positional inputs evaluate
once in source order. Structured output must round-trip through the same grammar;
malformed trees, forged internal markers and excessive/cyclic expansion reject.
Generated closures, nested macros, cleanup and required proofs use the existing
checker/evaluator/native path. Generator calls do not become runtime roots.
Formatting and editor result-type/definition queries recognize macro calls. See
the [macro example](../../compiler/examples/typed_macros).
`std.loom.syntax` emits grammar-validated source for files and individual
fragments. The [declaration tool](../../compiler/examples/ast_generation) builds
AST declarations in Loom, then produces a normal package that passes its own
check/test/run and required proof. Top-level `comptime` blocks now also generate
declarations in memory before ordinary binding and checking. They return Text or
AST through pure functions in the original source universe, without implicit
iteration or generated imports. Generated symbols retain package visibility,
required proofs, test isolation and distinct closure/macro identities. Editor
queries retain the raw source basis, navigate to the generating block, and
revalidate tracked inputs. Declaration expansion reruns before per-definition
matching, so unchanged generated bodies reuse resident or persistent checks while
changed output invalidates consumers. Snapshots retain validated raw source and
private expanded trees, including expansion identities; reused generated bodies
use the current generating block's extent. See the
[in-compilation example](../../compiler/examples/declaration_generation).
`std.reflect.predicates.describe` now returns visible direct refinement predicates
as canonical source and public AST data, including generated declarations. It
retains lexical privacy, declaration-site names and fresh mutable descriptor
storage. It does not expand helper bodies, grant private access, retain native
helper roots or supply proof evidence. See the
[predicate example](../../compiler/examples/reflection/predicates.loom).
First-class compile-time type values now use the keyword `type`. Pure functions
can pass, return, compare and select identities, including through compile-time
data and loops. Known type bindings supply local annotations, generic arguments
and visible nominal constructors and exact-type record/enum patterns. Static
field and type-pack iteration uses these same operations after selecting a known
shape; unknown generic shapes do not gain undeclared construction capabilities.
Native code retains only the selected types.
Runtime escape rejects, and static aggregate keys preserve exact type identity.
Type construction requires explicit staging; declaration signatures still use
ordinary generic parameters. See the [example](../../compiler/examples/type_values).

Public `std.loom.parser` now parses standalone expressions, types, match/binding
patterns, statements and declarations through the existing grammar. Fragments
must consume their complete input and retain its original UTF-8 spans. Pure
runtime and compile-time clients use the same source API, without synthetic
wrappers or compiler subprocesses. This supplies syntax data, not macro expansion
or successful semantic checking; see the
[fragment example](../../compiler/examples/syntax/fragments.loom).

Stable schemas, lossless editing, richer pack iteration, broader compile-time
reflection, and contract reasoning remain in the
[roadmap](../../ROADMAP.md#n2--complete-the-language-and-source-library).

`comptime` parameters specialize named calls using scalar, immutable aggregate or
source-function identities; static values and function identities disappear
before the native ABI. Inferred generic and associated parameter types now
specialize to those same supported shapes, including concrete/dynamic methods;
unsupported concrete shapes reject before evaluation/emission. Generic static
values remain abstract during declaration checking, just like callbacks.
Float keys use CTFE's round-tripping encoding rather than IEEE equality, keeping
signed zeros distinct and reusing the existing canonical NaN encoding. This
does not add a runtime argument, numeric representation or Float proof rule;
the [Float example](../../compiler/examples/comptime_parameters/floats.loom)
covers static packs, forwarding, closures and associated dynamic methods.
Records, tuples, enums and constrained values use framed structural keys, retaining
nominal type, enum tag, field order and Text/Float distinctions. Equal configurations
reuse a specialization even when constructed differently. All stored fields and
enum variants must be immutable; List/Bytes, Task, dyn values and stored callbacks
are excluded. MustScope obligations and constraint checks remain unchanged.
The [aggregate example](../../compiler/examples/comptime_parameters/aggregates.loom)
covers static/dynamic calls, refinement widening, nested iteration and closures.
Known functions use determined targets; generic references, pure selectors and forwarding preserve
type checks and target preconditions. Static-value branches are checked with
abstract type arguments and declared requirements, not incidental concrete
conformances. Unknown runtime inputs and unproved abstract postconditions reject.
The [static-parameter example](../../compiler/examples/comptime_parameters/main.loom)
also exercises callbacks returned by specialized selectors. References
to static-parameter declarations remain unsupported.
Captured parameters now pass only a typed managed environment. Construction
materializes once, forwarding shares current state, and returned closures retain
it. Captured contents do not create extra native specializations of the same
target/layout. Static/dynamic methods and async calls share the existing ABI;
ordinary runtime bindings still cannot enter compile-time execution. Constructing
effectful or async references is distinct from calling them: indirect compile-time
calls check all discovered targets of their shape, including unexecuted runtime
branches. Nested staging cannot read a live parameter environment; whole-call
compile-time execution remains available. See the
[captured parameter example](../../compiler/examples/comptime_closures/README.md).

The [lexical cleanup example](../../compiler/examples/cleanup/main.loom) runs
late-bound `defer` blocks on normal, tail, return, `Result?`, `break` and `continue` exits, with LIFO
order and managed result snapshots. Pure cleanup shares compile-time execution.
Native stack registrations also drain synchronous language faults, preserving
the first diagnostic and remaining cleanups if a callback faults. This terminates
the process without exception unwinding; OOM and external termination do not
guarantee cleanup. Synchronous `scoped` now reuses those registrations with
statically selected source Dispose methods. Resource-flow checking rejects
copying, escape and manual disposal, including transitive receiver calls;
ordinary shared fields retain their semantics. MustScope requires scoped
handling or fresh return, with immediate single-payload Result/Option transfer.
Runtime callbacks and dynamic factory methods preserve that result obligation:
all selected implementations must establish freshness, including targets reached
through stored or returned function values. Dispose-only callback results do not
imply freshness. Unused concrete resource functions are checked without entering
native reachability. Async functions use frame-backed registrations for suspended
cleanup. Multi-field MustScope aggregates disarm pending field cleanup after
successful construction. Nested record fields also register each completed
resource's cleanup before a later field is evaluated. Generic and associated
resource fields now use their declared Dispose/MustScope bounds, rebuilding all
descendant guards in each concrete instance. Native and compile-time examples
cover normal transfer and nested cleanup faults. Enum constructors now protect
completed payloads before later arguments execute. Scope cleanup selects the
active variant, with independent callbacks so an outer or child fault cannot skip
remaining payloads. Flat, nested and guarded scoped matches borrow payloads without
transferring cleanup ownership. Native O0/O2, compile-time, forced-GC, suspension
and cancellation tests cover this path. Resource Lists now support fresh literals
and `std.resource.generate(count, factory)` with runtime counts. Typed callbacks
drain elements in reverse order, including partially constructed Lists and nested
record/enum/List cleanup faults. Synchronous MustScope parameters are checked
borrows; indexing and read-only calls preserve the single cleanup owner. Native,
compile-time, moving-GC, suspension and cancellation tests cover this path.
Recursive resource layouts through Lists now use finite typed cleanup functions;
recursive factories and borrowed traversal share the same fresh/no-escape checks.
Native/compile-time resource trees and nested fault/cancellation tests cover this
path. Resource transfer into Tasks remains unsupported.

The [file-tool trial](../../compiler/examples/wordcount/README.md) exercises
same-directory tests, a separate library package, Unicode text and file I/O.
`loom init <name>` creates a fresh minimal directory package, entry point and
same-directory test; it rejects existing directories and invalid module names.
Development compiler paths resolve std/native from their checkout, allowing
check/build/test/run from the application's own directory; `run --` forwards
program arguments. The VS Code development host has a dedicated trial workspace.
Real host/protocol smoke tests cover the editing loop. Public/API rename, broader syntax-error
recovery and typed queries within erroneous functions remain programming-experience
gaps. Native assertions now carry static
definition-file/line/Unicode-column diagnostics. Test entries set one current
test name, retained with the first fault across cleanup. Standalone test binaries
need no source files, and production builds emit no test context or test-only
labels. Successful test summaries are unchanged. Other faults still lack source
locations; stack traces and continuing after a fault remain open.

Unlabeled loop control lowers directly to native branches and shares compile-time
execution, including cleanup at the nearest loop boundary. The
[loops example](../../compiler/examples/loops/main.loom) exercises check/build/test/run.
General loop-invariant proofs remain unsupported. List literals now use `[a, b]`
and context-typed `[]`, including generic, constrained and dynamic elements.
Runtime construction reserves the known capacity and writes elements directly;
moving-GC tests cover earlier element snapshots and nested sharing. General
compile-time graph materialization still uses allocate-then-fill construction.
List/Bytes subscript reads and writes now reuse checked native/evaluation
primitives. Shared `let` aliases can update elements; only rebinding needs `var`.
Binding distinguishes indexing from generic application, including indexed
function calls, and rejects genuinely ambiguous field/method calls. Tests cover
operand order, alias growth, moving-GC snapshots, bounds/range faults and cleanup.

Normal compiler iteration can use a [single development rebuild](../../compiler/README.md#build-and-try-it);
CI retains full bootstrap generation checks. The initial
[latency harness](../../compiler/README.md#compiler-latency) measures fresh-process
check/build runs with warm OS caches and backend phase timings, including optional
cache comparisons. Measurements do not establish a performance target as achieved.
Native commands now offer an opt-in trusted-local object cache. The Loom driver
keys exact checked bytes and the backend's content/configuration identity,
verifies object/link metadata as one checksummed bundle, and always relinks
executables. With the object-cache option alone, source/type/proof checks still
run; IR requests bypass object caching.
The Rust bridge shares target configuration between identity and actual emission,
hashes the native executable and loaded LLVM implementation, and otherwise owns
only object emission and host linking. Runtime/linker changes do not reuse final
executables. This is not an authenticated binary cache or a frontend incremental
engine; unsupported implementation identity falls back to uncached compilation.
An independent [frontend cache](../../compiler/README.md#frontend-cache) now
persists successful whole-closure checks/lowered inputs. Every use reloads and
parses source and verifies dependency snapshots; keys include compiler bytes,
source membership/content/trust, module/import identity and test/check mode.
Checksummed metadata and artifacts publish together; damaged entries recheck.
Whole-closure hits skip type/proof/effect analysis and lowering, not actual test
execution, IR emission or final linking. On misses, a last-successful definition
snapshot can reuse ordinary/generated abstract checks and concrete bodies across processes.
It rebuilds bindings, invalidates changed/transitive consumers and remaps types,
calls and locations through the same resident-cache machinery. Compile-time
specializations retain detached immutable constants, including type values and
named callbacks; current types rebuild aggregate keys. Erased compile-time calls
still participate in source dependency invalidation.
Variadic bodies are keyed by original declaration and arity; current signature
expansion and arity validation precede reuse, including compile-time loops/maps.
Compiler bytes, mode/options, module/import identities and observed inputs bind the complete
checksummed snapshot. Declaration expansion reruns before matching; its raw
source and expanded trees have separate private snapshots. Unsupported snapshots
and damaged bundles miss; failed checks never publish. This is opt-in trusted-local evidence, not a
portable proof or authenticated remote cache. Native objects remain whole-closure.
`std.build.input_file(comptime path Text) Text` now embeds tracked UTF-8 snapshots
relative to the declaring package, confined to its module. Reads happen during
checking and lower to ordinary Text constants, with no new runtime/backend ABI.
Frontend hits revalidate actual resolution/content; build receipts bind
requests and digests. CLI/editor explicitly supply a filesystem reader; public
checking/analysis defaults remain filesystem-free and can consume detached
in-memory snapshots. See [the example](../../compiler/examples/build_inputs/main.loom).
`std.build.input_bytes` now shares that raw-byte tracking path and returns fresh
mutable Bytes. Invalid UTF-8 still rejects at text requests, not binary reads.
Binary inputs and compile-time-produced buffers use compact checked literals,
static LLVM data and one private bulk-copy allocation boundary, retaining aliases
within computed graphs. Native O0/O2 and forced-GC tests cover isolation, suspension,
compact IR, same-size cache invalidation and receipts; editor watches include
binary changes. Source `std.encoding.hex` supplies encoding/strict decoding.
Explicit `--build-option name=value` inputs now share this checking basis.
`std.build.option` distinguishes missing from empty values and has an ordinary
source fallback overload. Queries become constants before emission; no environment
lookup or runtime operation is added. Options are application-wide, detached and
canonicalized; duplicates reject. CLI and VS Code `loom.buildOptions` feed the same
checks, member completion, hover, rename and import validation. Frontend-cache
keys and public analysis freshness include the full option map. V3 receipts bind
option names and value digests; values can still enter generated artifacts and
must not contain secrets.
`std.build.target` now supplies actual backend OS, architecture, pointer width
and byte order as compile-time Text. The CLI/editor read target metadata only
when observed; public analysis can supply a snapshot without a backend process.
Observed properties survive into checked artifacts, cache revalidation and the
receipt's checked-input digest. Emission rejects a mismatched target. This adds
optional backend-neutral metadata, not runtime platform dispatch, cross-compilation
or a new bootstrap checkpoint. Unsupported definition forms still recheck.
The [native basic benchmark](../../benchmarks/basic/README.md#managed-memory-lowering-repair)
also records the managed-memory lowering repair, including same-session baseline
comparisons for native kernels and whole compiler checks. Those measurements
describe the earlier nonmoving runtime; they do not establish the performance
of copying GC or complete the remaining resource/async design. A separate
[moving-collector comparison](../../benchmarks/basic/README.md#moving-collector)
records faster compiler-check CPU time but higher peak RSS, with noisy native
kernel samples. Memory efficiency and the remaining language work are not closed
by those results.

The separate `tools/semantic_change_trial` prototype initializes and refreshes
stable-ID sidecars for root-level, single-package source. It previews
move-plus-edit and one-sided-addition merges from directories or Git blobs and
applies an exact reviewed result to a new directory. Import-bearing packages
require an explicit pinned project context: the review token covers the selected
production/test closure and apply reloads it. Both the exact baseline and
proposed merge must analyze in that closure, so an incompatible dependency API
drift cannot be hidden by an adapting package edit. Import changes and general
cross-package edits do not merge. For supported move-plus-edit proposals, checked
reference targets in unchanged contributed declarations are compared using stable
sidecar identities or pinned dependency locations. A changed overload target is
rejected; missing or ambiguous evidence and unsupported source forms fail closed.
See the [move](../../tools/semantic_change_trial/fixtures/left/app.loom) and
[edit](../../tools/semantic_change_trial/fixtures/right/value.loom) snapshots.
This does not prove behavior equivalence or a general semantic merge. Identity
refresh is explicit; it does not follow arbitrary edits automatically. Same-file
overloads now have parameter-type locators separate from stable IDs; body edits,
formatting and declaration movement do not select the wrong overload. `scan`
lists exact locators for explicit mappings. A stale locator rejects rather than
falling back to declaration order or name. The
[overload snapshots](../../tools/semantic_change_trial/fixtures/overloads/base/app.loom)
exercise moving one overload while independently editing it; overload additions
and unresolved binding changes still require review.

The `tools/deployment` prototype analyzes schema conflicts and executes one
bounded offline SQLite migration package with upgrade, rollback and re-upgrade
actions and append-only events. It provides read-only live preflights for the
fixed upgrade and downgrade sources; execution rechecks inside its write
transaction. A plan digest binds the declared action and inputs across retry.
Build receipts are reverified against artifact bytes, but
the operator still supplies the storage mapping; a receipt is neither a
signature nor proof that application code obeys that mapping. Read-only hotfix
inspection checks receipts, the starting basis and declared schema equality,
then reports compatibility as unproven and cannot execute a hotfix.

Accepted language and deployment decisions remain targets, not claims that the
whole design is implemented. Bootstrap agreement is not a correctness proof.
No release or complete standard library is claimed; additional host/architecture
combinations remain unvalidated.
