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
Async concept/impl methods now share the same constructor/resume path through
concrete, generic and dynamic calls. Implementations must match the declared async
effect. Default methods, associated results and type/comptime method parameters
reuse ordinary specialization. Dynamic calls also transfer direct Task parameters
and results through synchronous methods, without adding an owner or runtime ABI.
Witnesses retain only used constructors; child failures preserve dynamic creation
locations. Scoped receivers cannot escape into Tasks; async MustScope/NoSuspend
parameters remain rejected. See the [method example](../../compiler/examples/async_methods).
Named async references and synchronous Task factories now share structural
`fn(A) Task[B]` values. Parameters, returns, records and Lists store a code pointer,
not a live Task. Calls retain owner checks, one-shot transfer and the actual
indirect creation location. Only Task-returning callback signatures gain a private
label argument; synchronous factories use one adapter per referenced target.
Ordinary callbacks retain their ABI. Compile-time Task references and capturing
closures remain unsupported. See the [callback example](../../compiler/examples/task_callbacks).
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
Tuple joins, socket adapters and general worker APIs remain unfinished.

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
close. Focused tests exercise actual timers and localhost sockets. Source timer
Tasks now use this notification path. `std.file.tasks` adds byte/text reads and
writes using lazily created native workers, capped at four threads per owner.
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
assertions and lexical branches. The original guard remains while proved
constructors lower directly, preserving one input evaluation. Mutable bindings,
heap reads, cross-local relations, branch joins and general Float reasoning
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
compile-time and suspended execution. Literal/record patterns and guards remain
open. Compiler production sources retain flat patterns.

Named function values have structural signatures, contextual overload/generic
selection, and native calls through one code pointer. Parameters, returned
callees and aggregate storage share the ordinary ABI and GC rules. Pure
compile-time invocation and returned-reference reification use the same checked
model. The [callback example](../../compiler/examples/callbacks/main.loom) runs
under forced collection; O0 scalar callbacks have no Loom runtime dependency
and retain only referenced targets. Capturing closures are not yet implemented.

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
cross-dyn conversions remain unsupported.

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

Methods now accept Int/Bool/Text and function `comptime` parameters, with matching
positions in the concept and implementation. Static and dynamic calls share
selected-branch validation under declared bounds; known implementations in unused
bodies cannot hide an invalid selected branch. Overrides do not instantiate the
default body they replace. Dynamic slots distinguish static
values and omit those arguments from their native signatures. Default forwarding,
generic methods and static-method compile-time execution use the same checked
model. Concept contracts remain unsupported.

Pure compile-time dynamic construction/calls now use those same checked witnesses,
including associated, generic, default and static method instances. Purity checking
follows all possible targets of a called interface slot, retaining the ban on I/O
even in an unexecuted runtime branch. Reified dyn results preserve admitted evidence,
receiver values and shared/cyclic containers while rebuilding witnesses in the
output queue; compile-time-only methods do not become runtime roots. The
[dynamic computation example](../../compiler/examples/comptime_dynamic/main.loom)
also runs natively under forced moving collection. Runtime capture and required
proofs over dynamic values remain unsupported.

Source `std.int.parse` handles signed decimal input and range errors without
runtime parsing helpers; the same function can execute at compile time. The
[arguments example](../../compiler/examples/arguments/main.loom) combines it
with constrained construction, command-line input, and ordinary source tests.

`Int` and `Float` constraints may call pure helpers with loops, recursion, and fresh data.
Their supported operation/call closure is validated even for unused constrained
declarations. Known true/false predicates remove the check or reject; unknown
inputs or unsuccessful optional evaluation retain normal runtime construction
and fault behavior. Function contracts now reuse direct acyclic scalar helpers
with immutable locals, `if/else`, and tail expressions or early body returns. A private checked closure
preserves helper preconditions, eager/unused arithmetic and short-circuit guards;
successful entry checks provide facts, while exit checks must be proved. Helpers
used only by postconditions stay out of native reachability. The
[contract example](../../compiler/examples/contracts/README.md) exercises this
through the CLI and compile-time execution. Generic declarations still require
abstract proofs. Direct scalar calls in a body requiring proof now compose
verified callee postconditions, or expand a finite pure body without a summary.
Argument snapshots preserve eager evaluation; proof-only temporaries do not
change emitted calls or locals. Conditional scalar results reuse existing typed
branches for body proofs and bounded Boolean normalization for contracts. Reversed
linear relations and excluded integer interval endpoints retain branch facts.
Recursive proof dependencies, helper loops/mutation, returns inside helper operands
and indirect/dynamic calls remain unsupported; required proofs never
fall back to runtime checks or sampled evaluation.

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

The Windows x64/MSVC implementation now includes native linking, binary file
I/O with Unicode paths/arguments, drive/UNC/verbatim package paths, and native
artifact suffixes. Its [CI job](../../.github/workflows/ci.yml) passes native
bootstrap and all workspace tests, including compile-time recursion budget
errors, GC, and redirected I/O. Cold bootstrap consumes a trusted checked
compiler export from the same workflow's validated macOS checkout, builds a
native Windows stage 0, then runs the regular stage 1/2/3 gate. This temporary
transfer is neither a committed IR snapshot nor a second language frontend;
`emit-checked` still enforces source types and required proofs.

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
produce no result. Rename and incremental semantic caching remain unimplemented.
Qualified paths now enumerate existing package/import spellings, preserving
overloads and source-instance/test identity. Local receiver bindings take priority;
type and dyn positions filter declarations without claiming valid instantiation.
Import-path discovery and automatic imports remain open.
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

Stable schemas, lossless editing, variadics, typed macros, broader compile-time
reflection, and contract reasoning remain in the
[roadmap](../../ROADMAP.md#n2--complete-the-language-and-source-library).

`comptime` parameters specialize named calls using canonical Int/Bool/Text or
source-function identities and disappear before the native ABI. Known functions
use direct calls; generic references, pure selectors and forwarding preserve
type checks and target preconditions. Static-value branches are checked with
abstract type arguments and declared requirements, not incidental concrete
conformances. Unknown runtime inputs and unproved abstract postconditions reject.
The [static-parameter example](../../compiler/examples/comptime_parameters/main.loom)
also exercises callbacks returned by specialized selectors. References
to static-parameter declarations, capturing closure parameters and variadics
are not included in this slice.

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
cleanup; nested resource aggregates and transfer into Tasks remain unimplemented.

The [file-tool trial](../../compiler/examples/wordcount/README.md) exercises
same-directory tests, a separate library package, Unicode text and file I/O.
Development compiler paths resolve std/native from their checkout, allowing
check/build/test/run from the application's own directory; `run --` forwards
program arguments. The VS Code development host has a dedicated trial workspace.
Real host/protocol smoke tests cover the editing loop. Import-path discovery, broader syntax-error
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
check/build runs with warm OS caches and backend phase timings. It does not
implement incremental frontend reuse or establish a performance target as achieved.
Native commands now offer an opt-in trusted-local object cache. The Loom driver
keys exact checked bytes and the backend's content/configuration identity,
verifies object/link metadata as one checksummed bundle, and always relinks
executables. Source/type/proof checks still run; IR requests bypass caching.
The Rust bridge shares target configuration between identity and actual emission,
hashes the native executable and loaded LLVM implementation, and otherwise owns
only object emission and host linking. Runtime/linker changes do not reuse final
executables. This is not an authenticated binary cache or a frontend incremental
engine; unsupported implementation identity falls back to uncached compilation.
The [native basic benchmark](../../benchmarks/basic/README.md#managed-memory-lowering-repair)
also records the managed-memory lowering repair, including same-session baseline
comparisons for native kernels and whole compiler checks. Those measurements
describe the earlier nonmoving runtime; they do not establish the performance
of copying GC or complete the remaining resource/async design. A separate
[moving-collector comparison](../../benchmarks/basic/README.md#moving-collector)
records faster compiler-check CPU time but higher peak RSS, with noisy native
kernel samples. Memory efficiency and the remaining language work are not closed
by those results.

Accepted language and deployment decisions remain targets, not claims that the
whole design is implemented. Bootstrap agreement is not a correctness proof.
No release or complete standard library is claimed; additional host/architecture
combinations remain unvalidated.
