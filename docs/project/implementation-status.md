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
| Memory/resources/async | Moving GC, lexical cleanup, stackless Tasks, real timers, files, DNS/TCP/TLS and tuple/List joins. | Cooperative scheduling; concurrent TLS directions and general worker APIs remain open. |
| Programming tools | Directory tests, recursive test selection, formatter, LSP/VS Code, public parser/analysis libraries. | Public/API rename and broader erroneous-source queries remain open. |
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
completion, checked import fixes, parameter/local-pattern rename and narrow private-function
rename. Resident workers reuse valid analyses and definition checks while
revalidating loaded source and observed inputs. Completion recovery is virtual;
it supplies neither build success nor proof evidence.
Runtime parameter declarations and match payload/whole bindings retain exact
checked definition locations. Renames include contracts, preserve shadowing and
recheck the virtual package. Public API/compile-time parameter rename and broader
recovery/query support remain open.

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
compile-time-only. Static scalar/immutable-aggregate/function parameters,
selected branches, top-level variadics and fixed-shape `comptime for/map` share
ordinary typing. Heterogeneous tuple packs are not runtime-sized Lists.
Method/data packs, multiple packs and universal variadic postcondition proofs
remain unsupported. Compile-time execution is not proof by sampling.

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
value predicate uses checked Result construction. Safe weakening and supported
implications avoid redundant checks while retaining input evaluation and faults.
Predicates may call pure functions, but cannot rely on external mutable state.

Every declared `ensures` requires a static proof. Current reasoning includes
scalar/inline-aggregate identities, guarded preconditions/assertions, bounded
integer difference relations, input-type invariants, inferred scalar loop
invariants, finite pure helpers and verified callee summaries. Synchronous dyn calls use declared method contracts,
not hidden receiver knowledge. Unsupported or exhausted required proofs reject;
they never become runtime postcondition checks. `old` currently covers immutable
parameter scalar paths, not arbitrary entry-state snapshots.

Immutable observed record fields can coexist with unobserved mutable siblings.
Fixed-shape List views retain element identities across removal/regrowth,
moving GC and suspension. Explicitly isolated Lists with immutable elements
support constrained construction; copies of the constrained value share.
Length-only predicates permit element writes, and bounded preservation proofs
admit some appends. Content-dependent predicates retain read-only storage.
There is no implicit copy, monitor or alias-triggered runtime failure.

Scalar loops infer entry/guard bounds and check inductiveness to a fixed point;
zero-iteration paths and early returns retain separate obligations. This supports
scalar assignments, branches, nested loops, break/continue paths and existing
direct-call proof rules. Scalar lexical cleanup retains its checked order and
return snapshots; calls in guards, resource cleanup and heap mutation remain
unsupported. See the
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
`std.net.tls` supplies verified client/server streams, custom PEM roots, ALPN,
binary I/O and TLS close-notify. Rustls owns protocol/cryptography in a separately
linked provider; Loom owns TCP, suspension and cancellation policy. It verifies
certificate chains, time and names, and distinguishes truncation from clean EOF.
The first API serializes I/O per shared connection; cancellation closes that
connection after draining its transport waits. O0/O2 and moving-GC tests cover
real encrypted traffic, rejected peers and independent Rustls interoperability.
Concurrent TLS read/write, mutual TLS, general workers, broader socket options,
address racing and parallel Loom execution remain open.
See [Tasks and I/O](../../compiler/README.md#source-tasks).

## Modules, caching and performance

Path and exact public HTTPS Git/fork dependencies support importer-local module
instances and offline locked builds. Git `subdir` selects a monorepo module and
binds its exact directory to the lock; modules at the same URL/commit share one
whole-repository snapshot. Sibling path dependencies stay within that snapshot.
`resolve` alone fetches sources. Cache validation checks actual bytes and
membership, not sidecars. Distinct source instances retain distinct nominal types.
Version normalization, authenticated transport and graph-wide fork policy remain open.

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
fresh processes and requires a whole-closure miss. On the recorded macOS compiler
workload, removing redundant snapshot allocations reduced cached edit median
latency from 2462 ms to 1543 ms and peak RSS from 2485 MiB to 1448 MiB. The latter
run's uncached path used 1953 ms and 854 MiB: cache reuse still costs memory, and
small packages can be slower. Caching remains opt-in. These are measured cases,
not a universal speedup or completion of the performance goal.
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
