# Loom roadmap

This is the implementation route for the accepted
[project goals](docs/project/charter.md),
[language foundation](docs/rfcs/language-foundation.md), and
[change/deployment design](docs/rfcs/change-and-deployment.md). It is not a
release schedule or an assertion that these capabilities already work.
Current evidence stays in the
[implementation status](docs/project/implementation-status.md).

## Implementation approach

Maintain one Loom-written frontend over an existing Rust LLVM binding. Keep
one checked semantic model for native compilation and compile-time evaluation.
An AST, a checked typed program, and LLVM lowering are the initial
boundaries; add another representation only for a demonstrated consumer.

Compile ordinary scalar operations and calls directly. Carry GC, fault, or
scheduler context only where required by effects; do not make ordinary functions
participate in a runtime dependency executor. Library policies resolve to Loom
definitions, not public-name compiler dispatch tables.

Reuse audited LLVM/platform code when it fits. Do not port the current layer
structure or maintain an interpreter/native feature matrix as a goal. A bounded
compile-time evaluator uses the same checked rules; it is not a second public
runtime backend. Remove replaced paths instead of adding compatibility adapters.

The [native compiler](compiler/README.md) now passes the N0 vertical-slice gate on
macOS, Linux, and Windows: real check/build/test/run, typed data, shared lists, source file I/O,
constrained construction, and bounded required proofs. N1 now has a
[Loom-written compiler](compiler/loom/README.md) producing successive native
compiler stages through the retained LLVM tool; later
milestones below remain exit criteria, not completion claims.

## N0 — A native vertical slice

Build the smallest useful source-to-native path on macOS first:

- directory packages, explicit imports, `pub`, and isolated test roots;
- functions, scalar values, records/enums, basic generics and overloads,
  control flow, and the shared-data semantics needed by the slice;
- a small source `std`, including the text/collection and real file-I/O
  facilities needed to write a compiler;
- constrained construction and a small sound postcondition prover: unsupported
  required proofs are explicit errors, never unchecked assumptions or runtime
  postcondition fallbacks;
- ordinary `loom check`, `build`, `test`, and `run` on the same native program.

Keep one representative application with a same-directory `*_test.loom` file
and an embedded `test fn`. Show that library builds exclude both forms of test
code. Include one boundary failure and one required-proof rejection. Inspect
one scalar/record hot path for unnecessary allocation or scheduler machinery.
Use real I/O and the necessary memory/resource substrate, not a mock executor
or a host-language implementation masquerading as source `std`.

## N1 — Move the compiler into Loom

Source handling, package loading, binding, typing, bounded required proofs,
and checked program construction now run as native Loom code. Stage 1 builds
stage 2, and stage 2 builds a byte-identical stage 3 on all three CI hosts, using the same
retained Rust LLVM/platform bridge. Stage 3 passes compiler, `std`, and example
tests; stages agree on selected type/proof failure diagnostics. The first
bootstrap gate below is met for this subset, not all of N2.

Stages are generations of a bootstrap run, not language versions or permanent
compiler tiers:

1. Stage 0 is an existing, validated Loom compiler. Without one, build it from
   a frozen Rust seed followed by pinned Loom source checkpoints in a bootstrap
   cache. Each checkpoint implements capabilities before its successor uses them.
2. Stage 0 compiles the current Loom compiler source into stage 1.
3. Stage 1 compiles the same source into stage 2; stage 2 produces stage 3.
4. Compare stage 2/3 artifacts and selected diagnostics; run compiler, `std`,
   and application tests with the resulting compiler.

The Windows bootstrap path builds its initial native compiler from a trusted
checked export of the same source checkout, then follows stages 1/2/3 on Windows.
CI transfers that temporary input from the validated macOS job in the same
workflow; no checked-IR snapshot or second frontend is maintained. Native MSVC
linking, Unicode/binary I/O, and Windows package paths pass the full native gate.

Keep the compiler and its production library closure on a conservative bootstrap
subset. Implementing a language feature does not justify using it in the compiler
or adding another source checkpoint. Raise the minimum seed only for a substantial
implementation simplification or measured performance benefit; batch necessary
upgrades instead of extending the recovery chain for each feature. New user
features and their test fixtures are not limited by this implementation policy.

The selected seed must support both source syntax and the library/checked-artifact
interfaces used by that closure. Verify new library adoption with the existing
seed first; change a required boundary in a verified step without permanent
compatibility adapters. A future pinned compiler artifact can shorten cold
recovery without restricting the language. Daily development uses one-stage
`--dev`; stage 2/3 comparison remains a bootstrap/CI gate, not a per-edit rebuild.

Remove the replaced Rust parser, binder, checker, and prover from the active
tree. The frozen history is a bootstrap input, not an old-language compatibility
policy or another frontend to extend. Do not add a bulky checked-IR seed
snapshot. The retained Rust LLVM binding, host linker, GC, and platform runtime
are separate implementation boundaries, not a permanent basic language version.
They may evolve for native code and platform facilities without duplicating
source-language analysis. Codegen consumes backend-neutral checked programs;
LLVM is a replaceable implementation, not part of the source language or its
proof rules. Bootstrap agreement is evidence, not a proof of
compiler correctness.

## N2 — Complete the language and source library

The first source Task slice implements hot child creation, postfix await,
one-shot obligations and a ready queue on one owner thread. Loom lowers
suspension into typed functions and GC-traced frames; ordinary functions gain
no executor or per-call persistent-root registration. Resume faults propagate
through awaits and cancel/drain queued or suspended descendants. See the
[example](compiler/examples/tasks).

Source `std.time` connects monotonic timer waits to that ready queue. Deadlines
are computed once, the reactor is created only on the first external wait, and
the idle owner blocks without spinning or creating per-task threads. Relative
sleeps start in their task body; the clock origin is process-local and scheduling
promises no fairness. See the [timer example](compiler/examples/timers).

Direct Task parameters and returns now support reusable functions and nested Task
results, with one-shot argument obligations and lazy returned-subtree handoff.
Sync helpers do not install an executor; they expose Task inputs or an output.

Lexical cleanup now survives suspension through frame-backed captures, with
children and waits drained before parent cleanup. `NoSuspend` remains an
independent capability restriction, without ownership syntax. See the
[cleanup trial](compiler/examples/async_cleanup).

Source `std.file.tasks` now supplies byte/text reads and writes through bounded
native workers and the existing completion reactor. Source loops retain partial-I/O,
UTF-8 and close policy; cancellation drains active OS calls before scope cleanup.
Open/create and normal close also use workers, with owned results until extraction
or cancellation and single-transfer close tokens. Private native waits suspend
their caller without child Tasks. Duplication and failure-cleanup close remain
synchronous; a stuck OS call can delay cancellation. See the
[file task example](compiler/examples/async_files).

Async methods now reuse typed constructors through concrete, generic and dynamic
calls. Declared async effects must match; defaults and associated/generic/comptime
methods keep ordinary specialization and sparse witnesses. Direct Task transfers
also work through synchronous dynamic methods. Scoped receiver transfer remains
unsupported. See the [method example](compiler/examples/async_methods).

Named async function values now use `fn(A) Task[B]`, like synchronous Task
factories. Callbacks can be copied, returned and stored without retaining a Task;
actual calls preserve owner requirements, creation locations and one-shot transfers.
Native callbacks remain one code pointer. See the
[callback example](compiler/examples/task_callbacks).

Task-bearing tuples and records now support whole-value transfer, independent
field consumption and tuple destructuring. Async calls adopt every Task field;
completed producers retain all returned subtrees until extraction. Ordinary
metadata remains readable after Task fields transfer. See the
[aggregate example](compiler/examples/task_aggregates).

Task-bearing enums now transfer their active payload through ordinary `match`,
including Option and Result propagation. Typed matches adopt/return only the
selected variant's Tasks; empty variants create no task obligation at runtime.
See the [enum example](compiler/examples/task_enums).

Task-bearing Lists now use source transfer operations with a shared header and
one dynamic obligation group. Typed helpers adopt/retain nested children, including
recursive enum/List layouts. Loops consume the group through actual break/return
exits. See the [dynamic list example](compiler/examples/task_lists).

The next async gates are socket readiness adapters, general worker
operations and joins. Task scheduling is cooperative, not parallel Loom threads.
The [accepted Task design](docs/rfcs/tasks.md) remains broader than this slice.
Private multi-task completion observation now retains terminal order, wakes
typed Loom frames and detaches selections without consuming their results.
Children register once; cancellation removes observations before parent cleanup.
Typed outcome extraction and explicit draining cancellation now support source
`std.task.outcome` and `cancel`, including no-result and Task-valued results.
Already-terminal children keep their result; cleanup faults become owned data.
The next boundary is source join policies: sequential awaits in input order
alone still cannot provide prompt cancellation when a different child faults.

Extend the self-hosted path with the remaining accepted capabilities:

- application-grade source collections, text, error, and I/O libraries;
- concept/dynamic dispatch, associated types, and precise reachability;
- compile-time value/type/function parameters, variadics, selected-branch
  instantiation, typed macro generation, and structured reflection;
- tracked build inputs and reusable proof/effect summaries;
- broader contract reasoning and fine-grained, alias-safe constrained mutation;
- moving GC, complete lexical resource cleanup, stackless Tasks, real timer/I/O
  registration, and source-level tuple/list task composition;
- module resolution, multiversion normalization, scoped fork policies, lockfiles,
  and incremental reuse tied to actual inputs.

Each addition must work through the native CLI and its `std` tests. Required
proofs remain mandatory even while the supported prover fragment grows. Exact
overload ranking, macro spelling, solver choice, artifact encoding, and runtime
layout belong to focused implementation designs, not new feature checklists.

Basic source Map/Set now reuse bounded concepts, private nominal storage and
shared Lists, including collision handling, removal, alias-preserving resizing,
snapshots and compile-time execution. Native forced-GC tests cover managed keys
and values. This is a library addition, not new compiler/runtime container
dispatch; broader application I/O and collection APIs remain incremental work.

Offline path dependencies now traverse importer-local manifest edges and reuse
canonical source roots, with instance-qualified package/test isolation. Distinct
roots with the same module name coexist without merging nominal types, scopes
or entry points. Exact HTTPS Git/fork sources now use the same selected-import
traversal, with explicit resolution, locked source edges and actual snapshot
content/membership verification. Local path inputs remain editable. Version
ranges/normalization, graph-wide fork policies and authenticated transport remain
open. Native commands now have opt-in, trusted-local whole-closure object reuse
keyed by checked inputs and actual backend/toolchain content. Source checks and
final links still run; persistent frontend/proof reuse and per-package objects
remain open.

Static nominal concepts now lower explicit implementations and bounded generic
method calls into the existing direct-call path. Conditional conformance queries,
method qualification and source `std.display` use the same model. Native `dyn C`
uses explicit conformance evidence, GC-owned receiver snapshots and sparse method
tables, including generic calls and managed aggregate storage. Static associated
types and bounded generic records/enums normalize under explicit declaration or
branch evidence. Concept default methods use abstract Self checking and the same
static/dynamic call paths; explicit overrides remain authoritative. Generic
implementations infer target parameters and recursively check prerequisites,
including associated bindings and dynamic witness calls. Potentially overlapping
implementations reject. Dynamic associated bindings now retain exact type
identity through generic substitution, boxing and sparse witnesses;
there is no runtime conformance registry or erased-value
type discovery.

Associated members also accept explicit concept requirements and default type
bindings. Implementations establish the requirements; generic/default method
bodies consume those promises without assuming an overridable default's equality.
Effective binding cycles reject. Generic associated members accept their own
parameters, inherited input bounds and defaults; applications normalize into
ordinary concrete types after input validation. Generic methods have independent
parameters and inherited concept requirements. Static calls specialize directly;
dynamic calls materialize finite, used method instances in ordinary sparse witness
slots. Dynamic associated type families remain open.

Native binary64 `Float`, source numeric parsing/conversion, compile-time execution
and Float-based constrained types now share the checked/native path. The compiler
adopts Float through a pinned source checkpoint without extending the frozen Rust
frontend. Exact held predicates/conjuncts now discharge redundant Float
refinement checks. General required Float postconditions still reject; integer
algebra must not stand in for IEEE floating-point proofs.

Lexical immutable-scalar facts from preconditions, assertions and branch guards
now also discharge construction checks. Mutable/alias facts, cross-local
relations and branch-join inference remain later work. Bounded expansion of
direct scalar helpers now supports refinement implication while retaining
evaluation and precondition obligations; general helper control flow remains open.

Required function contracts now reuse that bounded helper expansion, preserving
multiple parameter/result identities and guarded evaluation obligations. Pure
predicates remain normal source functions; proof-only callees do not become native
roots. Their own contracts and unused callers still undergo mandatory checking.
Direct scalar body calls now reuse verified callee summaries or finite pure
expansion, preserving eager argument values and the original native body.
Recursive proof dependencies, unexpanded helper control flow, indirect/dynamic
calls and required Float reasoning remain open.

Structural tuples, numeric projection, and plain-name destructuring now use the
native aggregate path, including generics, shared containers, and compile-time
results. More general patterns and runtime-sized task composition remain later
work; a tuple does not substitute for a dynamically sized List.

Named function values now support ordinary higher-order functions, structural
signatures, aggregate storage, exact-reference reachability and pure compile-time
invocation/reification. Capturing closures remain open; runtime callbacks are
distinct from compile-time parameters.

Compile-time parameters now specialize named calls with Int/Bool/Text values or
known source-function identities and leave only runtime arguments in the native
ABI. Static callbacks become direct calls; generic forwarding and pure selectors
preserve target preconditions and reject compile-time effects. Selected branches
retain explicit generic requirements and mandatory abstract proofs. Concept and
implementation methods use the same static parameters; dynamic slots include
static values in their identity and erase them from the runtime signature.
Selected bodies retain abstract checking without emitting unused callers. Function
references to partially specialized static declarations, capturing closures
and heterogeneous packs remain open.

Pure dynamic construction, calls and returned values now share the compile-time
evaluator, with exact associated/generic/static witness slots. Purity follows the
called slot's checked targets; reification rebuilds admitted witnesses without
leaking evaluation queues, preserving shared/cyclic receiver data. Dynamic
required proofs and cross-dyn conversions remain open.

Lexical `defer` handles block completion, return, `Result?`, `break` and `continue`,
preserving LIFO order and saved result values. Native stack registrations also
drain synchronous language faults before process termination, preserving the
first error if a cleanup faults. Only an explicit native resume boundary catches
faults; OOM and external termination do not guarantee cleanup. Synchronous `scoped` now selects the source
Dispose capability and checks resource escape; MustScope also rejects ordinary
bindings and discard. Direct factories and immediate single-payload Result/Option
transfer are supported. MustScope results retain their fresh-return obligation
through runtime function values and dynamic factory methods; every selected
implementation is checked. A Dispose-only callback result has no such guarantee.
Nested resource aggregates remain open; suspended lexical cleanup is implemented
as described above, not a general resource-transfer facility.

List literals now share typed/native/compile-time semantics. Runtime literals
allocate known capacity once and store elements directly; general compile-time
graph reification retains its alias/cycle-preserving allocate-then-fill path.
List/Bytes subscript reads and writes now lower to the same direct primitives as
source-library access, preserving shared aliases, operand order and fault cleanup.
Generic application and indexed callbacks are distinguished by binding, not naming
heuristics. This does not yet implement fine-grained constrained shared views.

Stop-the-world copying GC now rewrites precise typed roots and object fields,
preserving shared aliases, cycles and allocation-crossing expression snapshots.
Nonallocating functions remain root-free and collection adds no per-access
barrier. This completes moving-memory support for the current native layouts,
not general local root liveness or concurrent collection. Suspended Tasks now
use separate typed spill liveness and owner-scoped frame roots.

The early syntax portion of the
[compiler-library gate](docs/rfcs/language-foundation.md#compiler-libraries-and-tooling)
now has native evidence: an independent [user package](compiler/examples/syntax/main.loom)
imports `std.loom.parser` and `std.loom.ast`, parses in-memory source, inspects
declarations and spans, and reports a syntax diagnostic. It does not import
the compiler CLI, LLVM bridge, project loader, or compiler-specific runtime
hooks; the compiler uses the same source libraries.

The opt-in `std.loom.manifest`, `project`, and `binding` layers now share the
compiler's implementation. A standalone
[project inspector](compiler/examples/project/main.loom) loads a selected
package/import closure and reports declarations and visible overload candidates
without a compiler subprocess. These are program-local indices and name
candidates, not final call resolution or checked expression types.

The opt-in `std.loom.analysis` layer now exposes checked expression types and
concrete call targets from a detached source snapshot. An independent
[semantic consumer](compiler/examples/semantic/main.loom) uses the same checker
and required prover without the compiler CLI/backend. Queries cover concrete
instances, not every template or signature position; snapshot checks are not
incremental reuse.

Prioritize ordinary programming feedback alongside language work. Source
formatting now has a shared `std.loom.format` implementation and `loom fmt`
file, recursive, check-only, and stdin modes. The
[VS Code development extension](editors/vscode/README.md) connects unsaved
buffers to the same compiler for diagnostics, formatting, type hovers and checked
definition navigation. An actual VS Code extension-host smoke passes on macOS.
Completion, rename, and incremental
semantic reuse are subsequent tooling work, not completed language features.
Native failing assertions now report their source location and current test,
including helper assertions and standalone test executables. The first fault
remains authoritative across cleanup; this does not add stack traces or recovery.

Bounded [compile-time execution](compiler/README.md#compile-time-execution) now
uses the same checked model and a Loom-written evaluator. Explicit blocks
support pure calls, local loops/recursion, and value results; `comptime if`
selects one branch, including equality/inequality of nested generic types.
Boolean composition now combines those guards, concept queries, static Bool
parameters and pure computations with compile-time short-circuiting. Evidence
stays on the actual selected path; unknown choices and required proofs are not
resolved by guessing later operands.
Shared-container
results preserve internal aliases and cycles while constructing a fresh graph
on each runtime evaluation. Successful pure results can be reused within one
check; persistent/incremental reuse, variadics, typed macros, and broader
reflection remain incomplete.
`Int` type predicates can call pure helpers through the same bounded evaluator;
known constants remove checks, while unknown results retain the runtime
construction boundary. Execution never substitutes for a required proof:
function contracts use the documented symbolic proof fragment, including bounded
expansion of checked scalar helpers.
Explicit Int refinement conversion now reuses that fragment to eliminate a
destination check only when both truth and definedness follow from the source
predicate. Exact call-free conjunction reuse also handles Float without
arithmetic rewriting. Immutable local-flow facts and bounded direct scalar-helper
expansion extend this optional proof as described above; mutable/alias facts and
general helper control flow remain open.

Typed metaprogramming later reuses this infrastructure. Public analysis does not
freeze the schemas or complete
trivia-preserving editing, persistent identities, incremental reuse, or semantic
version-control tooling.

## N3 — Deliver semantic change and deployment workflows

Build the accepted tools on the compiler's identities, bindings, contracts,
effects, and immutable build basis. Reuse an existing version engine; do not
create a second authoritative source store.

Close the three stories in the change/deployment record: library evolution,
ordinary-source semantic changes and feedback, and deployed-state recovery.
Test move-plus-edit merging, changed overload bindings, incompatible tightening,
unknown deployment outcomes, and preservation/restoration of downgrade data.
Affected pure feedback updates must mark retained old results as stale and must
not replay external effects.
Reconciliation remains a library/system workflow with explicit effect safety,
not a replacement execution model for every function or Task.

## Delivery discipline

Use focused PRs and tests proportional to the changed boundary. Retain the
passing macOS, Linux, and Windows bootstrap/test gates. Establish release and
additional-target evidence before broader support claims. Do not
recreate a large dual-backend differential suite.

Fast compiler feedback is a core user-experience goal. Measure startup, check,
build, and test-compilation latency plus peak memory on representative growing
packages. Separate frontend, LLVM, and host-linker costs. Report process and OS
cache conditions; a warm rerun is not evidence of incremental compilation.
Use a single rebuild with an installed Loom compiler for normal iteration;
retain the full stage 1/2/3 comparison and test gate for bootstrap validation.

Also measure native scalar, record, and collection workloads. Investigate
generated code before introducing another optimization layer; no performance
target permits weaker contracts or cleanup.

Alternate backends, a stable FFI/plugin ABI, and cross-cutting/AOP composition
are separate future work. They do not block self-hosting. The superseded
compiler remains in Git history as reference material, not in the active tree
as a compatibility target.
