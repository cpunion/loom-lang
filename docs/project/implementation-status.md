# Implementation status

Only the native compiler under `compiler/` is maintained. Its
[guide](../../compiler/README.md) lists the tested subset and commands.
The former workspace compiler and its feature matrices have been removed.

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
checked arithmetic. Helper loops, mutation and indirect calls remain unsupported
for this optional proof; they do not become proof assumptions.

Structural tuples support positional access and plain-name `let`/`var`
destructuring, including nested types and `List`/`Result` elements. Native layout,
compile-time evaluation, reification, and public typed analysis use the same
aggregate semantics. The [tuple example](../../compiler/examples/tuples/main.loom)
checks evaluation order and shared-container results under GC stress.

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
and check boxing exactly. They use the existing native witness ABI; compile-time
dynamic execution and cross-dyn conversions remain unsupported.

Associated member bounds and default bindings extend that same model. Explicit
bindings override defaults; defaults keep their declaration's name scope and
normalize against the effective implementation map. Generic callers and default
methods use the declared member promises, but implementation establishment cannot
assume an unproved promise. Required dyn bindings remain explicit, exact and
bound-checked. Associated member type parameters are still unsupported.

Source `std.int.parse` handles signed decimal input and range errors without
runtime parsing helpers; the same function can execute at compile time. The
[arguments example](../../compiler/examples/arguments/main.loom) combines it
with constrained construction, command-line input, and ordinary source tests.

`Int` and `Float` constraints may call pure helpers with loops, recursion, and fresh data.
Their supported operation/call closure is validated even for unused constrained
declarations. Known true/false predicates remove the check or reject; unknown
inputs or unsuccessful optional evaluation retain normal runtime construction
and fault behavior. Function contracts remain call-free and proofs mandatory.

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

[Compile-time execution](../../compiler/README.md#compile-time-execution) uses a
bounded Loom evaluator over the native compiler's checked model. Explicit
blocks support pure calls, local mutation, loops/recursion, and scalar or
record/enum results, including shared lists/bytes. Every runtime evaluation
constructs a fresh graph with its internal aliases and cycles preserved; no
hidden mutable global is introduced. Successful pure results can be reused
within one check, not across builds. `comptime if` selects code using a computed
Boolean or type equality/inequality, including nested generic types. This is not
general type-valued computation or Boolean composition of type comparisons.
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
late-bound `defer` blocks on normal, tail, return and `Result?` exits, with LIFO
order and managed result snapshots. Pure cleanup shares compile-time execution.
Fault unwinding, task cancellation and `scoped`/`MustScope` remain unimplemented.

Normal compiler iteration can use a [single development rebuild](../../compiler/README.md#build-and-try-it);
CI retains full bootstrap generation checks. The initial
[latency harness](../../compiler/README.md#compiler-latency) measures fresh-process
check/build runs with warm OS caches and backend phase timings. It does not
implement incremental reuse or establish a performance target as achieved.
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
