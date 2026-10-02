# Implementation status

Loom has one self-hosted native compiler, a source-written standard library and
usable CLI/editor tooling. It is suitable for the documented programming trials,
not a complete implementation of the [project goals](charter.md).
The [roadmap](../../ROADMAP.md) gives the remaining gates; the
[compiler guide](../../compiler/README.md) is the detailed reference for today's
syntax, APIs and supported proof fragment.

## At a glance

| Area | Current evidence | Important boundary |
| --- | --- | --- |
| Native compilation | Real check/build/test/run; Loom stages 2/3 agree on macOS, Linux and Windows. | LLVM is the only backend; bootstrap agreement is not a correctness proof. |
| Language | Generics, concepts/dyn, associated types, closures, tuples, recursive List-backed data and pattern matching. | Not every accepted generic/pack combination is implemented. |
| Guarantees | Checked constrained construction, safe weakening, bounded mandatory postconditions and selected invariant-preserving operations. | General loop proofs and mutable-alias preservation remain open. |
| Metaprogramming | Pure compile-time execution, type/value/function parameters, packs, typed macros, reflection and tracked inputs. | Staging and visibility still apply; no arbitrary compile-time I/O. |
| Memory/resources/async | Moving GC, lexical cleanup, stackless Tasks, real timers, files, DNS/TCP/TLS and tuple/List joins. | Cooperative scheduling; general worker APIs remain open. |
| Programming tools | Directory tests, formatter, LSP/VS Code, public parser/analysis libraries, checked function/type/concept/record-field edits. | Unaccounted/generated uses, external-consumer API edits and broader erroneous-source queries remain open. |
| Evolution tools | Reviewed single-package semantic merge and a real offline SQLite migration trial. | Bounded prototypes, not general semantic VCS or deployment compatibility proof. |

## Build and programming experience

The [Loom frontend](../../compiler/loom/README.md) handles loading, syntax,
binding, types, contracts and checked-program emission. A Rust LLVM 22/Inkwell
tool lowers checked programs and links native artifacts; it does not parse or
check source again. Rust also supplies GC and narrow platform primitives. There
is no maintained interpreter or legacy universal-value backend.

An existing compiler builds the current source. Cold Unix bootstraps recover a
pinned seed from history; Windows starts from source-bound checked input that
Unix CI reproduces. Production compiler sources deliberately use a conservative
subset. Development uses a one-stage rebuild; CI retains the stage 2/3 gate.
See [bootstrap instructions](../../compiler/README.md#build-and-try-it).

A directory is a package, with `pub` controlling external visibility. Both
`*_test.loom` and embedded `test fn` can use their package's private declarations;
library compilation excludes them. `loom test --recursive` selects each package
as an independent test root, includes test-only packages and continues after a
package failure. It skips nested modules, hidden/build directories and directory
symlinks. Dependencies are still loaded without tests. `--no-run` emits separate
package executables. The compiler's integration gate discovers `std` packages
through this command instead of a hand-maintained list.

`loom init`, argument-forwarding `run --`, and shared `loom fmt` support the
[multi-package file-tool trial](../../compiler/examples/wordcount/README.md).
Formatting preserves comments/literal spelling and expands function/record
bodies. Record fields require newlines or semicolons; whitespace adjacency
rejects. Raw multiline Text is supported. No-result functions omit the return
type and final return; source `Unit` is rejected. A value must be used or
explicitly discarded, and Task/MustScope obligations cannot be discarded.

The [VS Code extension](../../editors/vscode/README.md) uses the real compiler
for unsaved diagnostics, formatting, hover/navigation, name/member/import
completion, checked import fixes, parameter/local-pattern rename and checked
function/type/concept rename. One public top-level function, record, enum,
constrained type or concept can be renamed throughout its local module, including import declarations,
checked signatures and field/payload annotations, constructors, enum patterns,
qualified calls, callbacks, embedded
tests and unopened test files. Each directory retains its private test scope;
the complete virtual edit is checked before offering it. Nested modules and
directory aliases are excluded, as in recursive tests. Unknown occurrences,
overloads and generated references refuse edits; dependency snapshots and
external consumers are not rewritten. Resident workers reuse valid analyses and definition checks while
revalidating loaded source and observed inputs. Completion recovery is virtual;
it supplies neither build success nor proof evidence.
Runtime parameter declarations and match payload/whole bindings retain exact
checked definition locations. Renames include contracts, preserve shadowing and
recheck the virtual package. Runtime-local renames also cover selected compile-time
branches when checked specializations account for every occurrence. Compile-time
parameter hover/navigation/references/rename use a fresh
optional binding trace before specialization erases uses; values, callbacks and
covered compile-time blocks/branches work without executable IR metadata. Hover
retains concrete parameter types and navigation follows the source binding,
including specialized callbacks.
Concrete annotations navigate through checked types and package bindings, not
same-spelled specialized parameters. Private nominal types use the existing
test-inclusive package edit check. Unobserved branches, packs, type aliases and
unaligned annotations still refuse edits. Record fields now use checked
receiver/initializer identities, including updates, explicit destructuring,
contracts and type-constraint templates. Distinct field owners and locals stay
unchanged; unaccounted labels or structural accesses in any module package block
the edit. Concept rename uses a lazy checker trace for bounds, `impl`, `dyn`,
associated-type qualifiers, `implements` guards and explicit method calls.
Each package must account for its own references, including tests; unvisited
branches refuse edits. The trace stays outside executable IR and build caches.
External-consumer API migration and broader recovery/query support remain open.

Public `std.loom` syntax, fragment parsing, project/binding and typed-analysis
libraries are the same implementation used by the compiler. In-memory syntax
and semantic consumers need no compiler process, filesystem or LLVM. IDs/spans
belong to a selected snapshot, not persistent source history or a stable schema.

## Language and compile-time programming

Native code supports Int/Float/Bool/Text/Bytes, records/enums, tuples, shared
Lists, overloads and generic data/functions. Recursive structures through Lists
work; infinite inline layouts reject. Destructuring, nested/literal/guarded
patterns, List indexing and new-value record updates use ordinary typed
operations. Record updates do not mutate the base or inherit its refinement
without validation. See the [implemented subset](../../compiler/README.md#implemented-subset).

Concepts require explicit conformances. Static/default/generic methods,
associated bounds/defaults/families and exact dyn bindings are implemented.
Dynamic calls use statically established evidence and sparse used slots, not
`any`-based runtime discovery. Explicit `impl D for dyn C` adapters work;
dynamic associated families and zero-allocation witness upcasts do not.
Named/captured callbacks retain typed environments; closures cannot conceal
live Task or scoped-resource obligations.

Compile-time execution uses a bounded evaluator over the checked model, with
pure functions, loops, recursion and fresh shared graphs. Type values remain
compile-time-only. Source `std.meta` exposes a function value or declared function
type's ordered parameter types and optional return type through ordinary Loom
helpers and library `Signature` conformance; callbacks are not
executed, captured data is not inspected, and an omitted result is `None`, not
`Unit`. Static scalar/immutable-aggregate/function parameters,
selected branches, function/data type packs and fixed-shape `comptime for/map` share
ordinary typing. Heterogeneous tuple packs are not runtime-sized Lists.
Static tuple/record type iteration also accepts immutable compile-time-selected
type bindings, with the same lexical visibility and ordinary generated code.
Record/enum packs infer arity from structural initializers, preserve nominal
identity and check selected arities with abstract elements. Recursive data,
Task payloads, compile-time values and incremental restoration use the same
typed aggregate path. Elementwise associated types/families retain declared
bounds and do not infer receiver types from projected results.
Function packs also infer arity from callback parameters/results and nested
tuple or nominal patterns; contextual function references and overloads use
ordinary inference after expansion. The
[native example](../../compiler/examples/data_packs/functions.loom) covers
shared captures and once-only input effects, without a runtime pack.
Packs used only in results or compile-time generation are also accepted. Expected
results select arity for ordinary/async calls, function references and static/dyn
methods; explicit arguments select type generators without sample inputs.
Unknown arity still rejects. The
[factory example](../../compiler/examples/data_packs/factories.loom) covers these
paths and shared generated callbacks through native check/build/test/run.
Method-local packs support default/override methods, ordinary generic impl
headers, associated projections, structural inference, CTFE and async. Family
headers match structurally for every arity; selected bodies are checked under
abstract declared bounds. Dynamic calls use finite ordinary slots, not a new
runtime pack ABI. The [method example](../../compiler/examples/variadics/methods.loom)
also checks sharing and once-only effects. Impl-header packs infer arity from
nominal, tuple and function target shapes, including nested/repeated expansions.
Selected arities check methods and associated bindings with abstract elements;
method-local packs retain their independent arity. Overlap checking preserves
expansions and conservatively rejects unknown intersections, without sampling.
Private cache recipes retain the original member and both arities. The
[impl example](../../compiler/examples/data_packs/implementations.loom) exercises
native/dyn calls, declared contracts, shared values, CTFE and async payloads.
Multiple packs in one parameter list
and universal variadic postcondition proofs remain unsupported. Compile-time
execution is not proof by sampling.

Independent nominal declarations do not share a global type-count budget.
Recursive type expansion stays depth-bounded; associated-evidence search has
a per-request work budget, independent of unrelated interned types.

Typed expression macros receive inferred schemas and return checked hygienic
Text/AST expressions. Explicit top-level declaration generation runs before
binding; it is not an implicit fixed-point expansion. Reflection exposes
visibility-filtered type graphs, field reconstruction and direct predicate
syntax, not extra access or conformance evidence. See
[compile-time programming](../../compiler/README.md#compile-time-execution).

Tracked text/binary inputs, explicit build options and observed backend target
properties bind checking, editor freshness, caches and build receipts. Ordinary
pure execution cannot hide external I/O. Reified mutable graphs are fresh per
evaluation and retain their internal sharing; static immutable values specialize
without runtime argument slots.

## Constraints and required proofs

Construction returns the refined value when established statically; an unknown
value predicate uses checked Result construction, including immutable Text
refinements with byte equality. Safe weakening and supported
implications avoid redundant checks while retaining input evaluation and faults.
Predicates may call pure functions, but cannot rely on external mutable state.

Every declared `ensures` requires a static proof. Current reasoning includes
scalar/inline-aggregate identities, guarded preconditions/assertions, bounded
integer difference relations, input-type invariants, inferred scalar/inline-aggregate loop
invariants, finite pure helpers and verified callee summaries. Finite helpers
support local reassignment, conditional expression operands and
short-circuit writes while retaining prior value snapshots and evaluation
obligations, including overwritten calculations. This adds no runtime proof
state and does not treat shared-storage mutation as pure.
Expression composition now shares operation descriptions for operand shape,
conditional evaluation, heap observations and totality. Theory-specific value
and safety rules remain separate from expansion and traversal; adding a primitive
does not require enumerating its conditional operand combinations. Correlated
guards normalize without a larger budget, while unrelated guards, eager faults
and entry-state bounds remain independent obligations. This is not unrestricted
theory combination or a complete proof procedure.
Bounded affine equality elimination now composes multiple established equations
and inequalities over the same mathematical scalar symbols, regardless of
whether they originated as parameters, fields, callee results or length
observations. Fraction-free positive scaling keeps relation signs and definedness
obligations; overflow, disjunctions, Text contents and invalidated List state
gain no invented evidence. Nonlinear and quantified proofs remain open.
Synchronous scalar
and inline aggregate concept calls use declared contracts through generic/associated or dyn receivers,
not a guessed implementation or hidden receiver knowledge. Private abstract
summaries are neither executable CTFE bodies nor native functions. Unspecified
leaves remain independent; shared siblings supply no content or alias facts.
Immutable Text values, exact literals and established equality facts participate
in required proofs, including Text leaves beside shared siblings. Distinct unknown
values never imply unequal contents. Established equality chains and inequalities
between equal-value classes compose; disjunctions and unequal chains supply no
invented equality or inequality. Immutable UTF-8 byte lengths, their nonnegative
bounds and equality consequences compose with entry values, pure helpers and
input-type constraints. Proved length-based weakening omits the second check;
unknown strengthening retains it. Equal lengths do not establish equal contents.
Bounded concatenation proofs preserve ordered contents, byte-length sums, empty
identity, reassociation and established atom equalities. Pure helpers, entry
values and constraint weakening compose without runtime proof allocations;
hypothetical sizes and eager helper obligations must still be proved. This is
not a word-equation solver. Substring reasoning is not implemented; Bytes remain
opaque.
List lengths belong to the current proof state: exact aliases share extent
updates, while unknown overlap and opaque calls forget current-length facts.
Scalar reads remain snapshots. Length helpers, literals/allocation, indexed
reads/writes and append compose with generic/inline values and bounded loops.
Fresh allocations are disjoint from existing handles; later unknown reads or
returns may alias them. Copy loops infer output-length/index equalities and
check them inductively, preserving input extents only with that separation.
Input length refinements supply entry facts; callee preconditions are not reused
as post-call heap facts. Element observations now track literals, appends, writes
and repeated reads at proved-equal indices, bounded to 64 per handle. Other
indices survive writes only with proved inequality; unknown handle overlap,
opaque calls discard content facts. Contracted length-preserving calls retain
untouched observations using bounded may-write analysis of their checked bodies,
including immutable parameter/inline aliases, nested calls and ordinary cleanup.
All branches contribute writes; mutable locals and heap-derived targets are
unknown. Changed helper bodies invalidate dependent cached proofs, even when
their declared contracts stay unchanged. No-result helpers with finite checked
effects need no additional contract to frame untouched storage.
Mutating loops use fresh element values and
retain only entry/backedge-proved bounds and source-contract relations when
lengths are preserved. Lost candidates trigger dependent rechecks; ordinary
cleanup and loop jumps retain their order. This covers bounded element counters
and two-element sum conservation, not arbitrary-index array invariants. Indexed entry
checks and storage postconditions compose; hypothetical postcondition reads
must prove bounds and have an established observation. Unknown reads never
supply non-aliasing evidence. Immutable entry-element snapshots require entry
bounds and survive writes, including through per-invocation summaries. Quantified
contents remain open. See the [indexed example](../../compiler/examples/list_elements) and
[List contract example](../../compiler/examples/list_contracts).
Unsupported or exhausted required proofs reject;
they never become runtime postcondition checks. `old` composes immutable entry
parameter paths, aggregates, arithmetic and finite pure helpers, retaining
definedness obligations. List entry lengths and immutable elements, including
pure helper observations,
survive mutation and compose using each callee's invocation state. Only scalar
and inline proof snapshots are created, with no runtime allocation. It cannot snapshot
mutable element handles or use callbacks or body-local/result bindings. Entry
element bounds may use immutable entry guards from short-circuit clauses and
pure helper branches. Post-state storage and results cannot justify entry reads;
conditional observations remain conditional after growth and across calls.

Immutable observed record fields can coexist with unobserved mutable siblings.
Fixed-shape List views retain element identities across removal/regrowth,
moving GC and suspension. Explicitly isolated Lists with immutable elements
support constrained construction; copies of the constrained value share.
Length-only predicates permit element writes, and bounded preservation proofs
admit some appends. Content-dependent predicates retain read-only storage.
There is no implicit copy, monitor or alias-triggered runtime failure.

Scalar, List and supported inline aggregate loops infer entry/guard bounds and
check inductiveness to a fixed point. Rebound List leaves supply length bounds,
not retained handle identities. Mixing
rebinding with possible resizing freshens all tracked extents before induction.
Zero-iteration paths and early returns retain separate obligations. This supports
whole-value record/tuple reassignment with independently fresh leaves,
branches, nested loops, break/continue paths and existing
direct-call proof rules. List extent-changing loops freshen affected lengths before
checking inductiveness. Scalar lexical cleanup retains its checked order and
return snapshots. Guard calls use fresh checked results, not stable syntactic
invariant terms. Resource cleanup and general heap-content proofs remain unsupported. See the
[native loop example](../../compiler/examples/loop_contracts).

General content-preserving mutation, strengthening existing mutable alias graphs,
general loop/recursive proofs, arbitrary `old` snapshots and general Float reasoning
remain open. Sorting/permutation contracts are an accepted goal, not a completed
story. Exact supported rules and examples are in the
[contract reference](../../compiler/README.md#contract-boundary).

## Standard library, memory and async

Source `std` includes text/numeric/byte operations, List algorithms, Map/Set,
Option/Result, typed JSON, storage descriptors/codecs, reflection, environment,
filesystem, process and I/O APIs. JSON has no special runtime implementation.
Collection hash/order laws remain caller obligations; streaming JSON, broad
iterator APIs and application-grade networking are not complete.

Stop-the-world copying GC preserves precise typed roots, sharing and cycles;
large-object storage is separate and stress tests relocate all sizes. Ordinary
nonallocating functions stay root-free. General local root liveness and
generational/concurrent collection are not implemented. Source exposes no
addresses, finalizers, weak references or ownership/borrow syntax.

Lexical `defer` and `scoped` handle normal exits, propagation, loop exits,
language faults, suspension and cancellation. MustScope freshness/escape checks
and typed cleanup cover nested records/enums/Lists and recursive resource trees.
Cleanup drains after secondary faults while retaining the first diagnostic;
OOM/external termination offer no guarantee. Scoped resources cannot transfer
into Tasks. See [cleanup](../../compiler/README.md#lexical-cleanup).

Stackless Tasks lower into typed state machines and GC-traced frames, using one
owner-thread ready queue. Only needed suspension state spills. One-shot handles
transfer through functions, callbacks, dyn methods and aggregates. Outcomes,
draining cancellation and tuple/List joins are source library policy over narrow
completion notifications. Tuple `.await` joins and scalar `.await?` preserve
ordinary typed result rules.
Source `std.task.deadline` composes cancellation with a monotonic deadline,
preserving completed results and draining both work and timer subtrees. It
requests cancellation, not a hard return-time guarantee for blocking OS calls.

Real timer/readiness/completion registration wakes the owner without per-Task
threads or busy polling. Nonblocking TCP, OS DNS, asynchronous files and process
capture are implemented; blocking I/O uses bounded native workers with copied native data,
not managed pointers. Cancellation of a running OS call waits for completion.
Process capture adds native pipe-drain threads and unbounded output buffering;
cancellation reaps the direct child without terminating process trees.
TCP exposes numeric local/peer endpoints, TCP_NODELAY and write half-close,
preserving pending receive registrations for EOF-delimited exchanges. Text-to-Bytes
encoding is source-library policy and returns an independent buffer.
Hostname connections interleave address families with configurable bounded
concurrency, stagger and a total cancellation deadline. Numeric address races
share that source policy. Sockets are registered before suspension, so completed
losers and unextracted child results cannot escape cancellation cleanup.
`std.net.tls` supplies verified client/server streams, custom PEM roots, ALPN,
binary I/O, required mutual certificate authentication and TLS close-notify.
Client/server identities and trust are explicit; verified peer leaf certificates
can be copied as DER for application policy. Rustls owns protocol/cryptography in a separately
linked provider; Loom owns TCP, suspension and cancellation policy. It verifies
certificate chains, time and names, and distinguishes truncation from clean EOF.
One reader and one writer can run concurrently per shared TLS connection, with
ordered encrypted writes and generation-checked completion wakes. Cancellation
retires the connection and its socket waits, waking the other direction to fail.
O0/O2 tests include simultaneous 8 MiB transfers, moving-GC traffic, rejected peers,
and independent Rustls interoperability with both-direction cancellation under
backpressure. Revocation policy, general workers, broader socket options,
and parallel Loom execution remain open.
See [Tasks and I/O](../../compiler/README.md#source-tasks).

## Modules, caching and performance

Path and exact HTTPS Git/fork dependencies support importer-local module
instances and offline locked builds. Git `subdir` selects a monorepo module and
binds its exact directory to the lock; modules at the same URL/commit share one
whole-repository snapshot. Sibling path dependencies stay within that snapshot.
`resolve` alone fetches sources. Cache validation checks actual bytes and
membership, not sidecars. Distinct source instances retain distinct nominal types.
Resolution defaults to anonymous; an explicit trusted credential helper enables
private HTTPS sources without persisting credentials or exposing remote output.
Real loopback HTTPS Git tests cover authenticated fetching, helper failures and
offline cache reuse. Root dependency entries with `scope = "graph"` explicitly
unify matching declared edges on one path/Git source; transitive declarations
cannot select another application's graph policy or grant extra imports.
Effective Git choices are locked per importing edge. Explicit SemVer resolution
discovers lightweight/annotated tags, checks module versions and both original
and graph-selected requirements, and searches candidate-dependent import closures.
Overlapping ranges prefer a shared source instance; disjoint ranges retain distinct
nominal types. Locks pin requests, labels, commits and verified content. Ordinary
commands remain offline. `std.semver` owns parsing/range policy; no runtime or
host-language resolver is added. Conflicting-graph search can be combinatorial.

Opt-in trusted-local object reuse hashes checked input/backend identity and
always relinks. Frontend reuse binds compiler bytes, loaded sources, modes,
options and observed inputs; discovery/parsing still run. On closure misses,
resident or persistent definition evidence can reuse eligible abstract checks
and concrete bodies with current bindings, types, calls and source locations.
Generated/staged/variadic/method/dyn/async/scoped bodies have native regressions.
Captured-frame bodies rebuild their environments; expression-macro consumers
remain conservative. Failed checks do not publish evidence. Native objects are
still whole-closure, and trusted-local snapshots are not portable proof artifacts.

The [edited-source benchmark](../../compiler/README.md#frontend-cache) uses
fresh processes and requires a whole-closure miss. One-shot CLI checks serialize
and retire temporary source/body aliases before returning mutable results;
resident editor caches still keep isolated copies. Body eligibility and concrete
callee reconstruction are memoized only within the current check, never across
edits. Nine alternating pairs of O2 compilers on identical edited macOS inputs
measured 1750/1614 ms and 1463/1187 MiB before/after this replay change, with the
same reuse counts. A separate uncached pair measured 1251/1247 ms. Restoration,
replay and writeback remain expensive; caching stays opt-in. This is not a
universal speedup or completion of the compiler-latency goal.
Separately, five alternating fresh-process checks of the same compiler sources
measured 2749 ms / 779 MiB before and 1188 ms / 486 MiB after removing repeated
trusted `Result` discovery and package-label allocations. No incremental cache
was enabled. Verified source shapes are reused only within the current check;
an edited binding snapshot validates them again. This is one macOS workload,
not a latency or memory guarantee for other projects.
See [native benchmarks](../../benchmarks/basic/README.md) separately; compiler
latency is not interpreter or application runtime performance.

## Semantic changes, deployment and delivery

The [semantic change trial](../../tools/semantic_change_trial/README.md) uses
explicit stable-ID sidecars and directory/Git snapshots for reviewed move-plus-edit
and one-sided additions. Pinned contexts and checked reference targets prevent
silent binding drift. Applying creates a new tree; arbitrary edits, cross-package
merges and automatic identity tracking are not implemented.

The [SQLite migration trial](../../tools/deployment/sqlite_migration_trial/README.md)
executes one offline orders upgrade, data-preserving downgrade and re-upgrade.
Append-only events distinguish assumptions and observations; plans bind retry
inputs, preflights recheck within transactions, and artifact receipts are rehashed.
The operator still supplies storage mappings. Hotfix inspection reports proof
as unknown and cannot execute a hotfix. General migration packages, application
mapping proof, online coexistence and generalized recovery remain goals.

CI packages relocatable development archives for macOS/Linux/Windows and tests
them outside the checkout. Host LLVM/linker dependencies remain external; these
are not self-contained published releases or evidence for other targets.

New status updates should replace the relevant boundary above, not append a
second completion narrative. Preserve detailed semantics in the compiler guide,
examples and accepted design records; preserve historical progress in Git/PRs.
