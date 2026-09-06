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

Explicit Int refinement conversions also remove a destination check when the
source predicate proves its truth and arithmetic definedness. The bounded,
call-free implication proof preserves one evaluation of the input; unknown
cases retain `Result` construction. It does not yet propagate local branch facts
or prove implications involving Float or helper calls.

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
and retain only referenced targets. Capturing closures and compile-time function
parameters are not yet implemented.

Source `std.list.map/filter/fold` use those function values without new runtime
operations. They traverse the initial index range in order, preserve shared
element semantics, and execute at compile time with pure callbacks. A pinned
function-capable source checkpoint precedes this standard-library adoption.

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
Generic conformances and dynamic associated bindings remain open; compile-time
dynamic execution and cross-dyn conversions are not supported by this slice.

Source `std.int.parse` handles signed decimal input and range errors without
runtime parsing helpers; the same function can execute at compile time. The
[arguments example](../../compiler/examples/arguments/main.loom) combines it
with constrained construction, command-line input, and ordinary source tests.

`Int` and `Float` constraints may call pure helpers with loops, recursion, and fresh data.
Their supported operation/call closure is validated even for unused constrained
declarations. Known true/false predicates remove the check or reject; unknown
inputs or unsuccessful optional evaluation retain normal runtime construction
and fault behavior. Function contracts remain call-free and proofs mandatory.

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

Normal compiler iteration can use a [single development rebuild](../../compiler/README.md#build-and-try-it);
CI retains full bootstrap generation checks. The initial
[latency harness](../../compiler/README.md#compiler-latency) measures fresh-process
check/build runs with warm OS caches and backend phase timings. It does not
implement incremental reuse or establish a performance target as achieved.

Accepted language and deployment decisions remain targets, not claims that the
whole design is implemented. Bootstrap agreement is not a correctness proof.
No release or complete standard library is claimed; additional host/architecture
combinations remain unvalidated.
