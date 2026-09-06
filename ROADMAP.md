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
   a pinned historical commit and its frozen Rust seed in a bootstrap cache.
2. Stage 0 compiles the current Loom compiler source into stage 1.
3. Stage 1 compiles the same source into stage 2; stage 2 produces stage 3.
4. Compare stage 2/3 artifacts and selected diagnostics; run compiler, `std`,
   and application tests with the resulting compiler.

The Windows bootstrap path builds its initial native compiler from a trusted
checked export of the same source checkout, then follows stages 1/2/3 on Windows.
CI transfers that temporary input from the validated macOS job in the same
workflow; no checked-IR snapshot or second frontend is maintained. Native MSVC
linking, Unicode/binary I/O, and Windows package paths pass the full native gate.

The minimum bootstrap language subset constrains the compiler's own source,
not which features it can implement for user programs. Implement a new feature
using the preceding stage's supported subset before adopting that feature in
the compiler source itself. The selected seed must also support the library and
checked-artifact interfaces used by that bootstrap; change those boundaries in
verified steps, not through permanent compatibility adapters. Advance the seed
after a verified bootstrap, using a pinned release/artifact when available.
No release is required for the
current historical-source fallback.

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

Native binary64 `Float` and source numeric parsing/conversion now have a
bootstrap-capable implementation. Float compile-time execution and constrained
bases remain the immediate next boundary; integer algebra must not stand in for
IEEE floating-point proofs.

Structural tuples, numeric projection, and plain-name destructuring now use the
native aggregate path, including generics, shared containers, and compile-time
results. More general patterns and runtime-sized task composition remain later
work; a tuple does not substitute for a dynamically sized List.

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

Bounded [compile-time execution](compiler/README.md#compile-time-execution) now
uses the same checked model and a Loom-written evaluator. Explicit blocks
support pure calls, local loops/recursion, and value results; `comptime if`
selects one branch, including equality/inequality of nested generic types.
Shared-container
results preserve internal aliases and cycles while constructing a fresh graph
on each runtime evaluation. Successful pure results can be reused within one
check; persistent/incremental reuse, variadics, typed macros, and broader
reflection remain incomplete.
`Int` type predicates can call pure helpers through the same bounded evaluator;
known constants remove checks, while unknown results retain the runtime
construction boundary. Execution never substitutes for a required proof:
function contracts still use the documented call-free proof fragment.

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
