# Loom roadmap

The [charter](docs/project/charter.md),
[language foundation](docs/rfcs/language-foundation.md), and
[change/deployment design](docs/rfcs/change-and-deployment.md) define the goals.
This roadmap defines delivery gates, not a second language specification or a
chronological changelog. See [implementation status](docs/project/implementation-status.md)
for current capabilities, evidence, and limitations.

## Implementation approach

Maintain one Loom-written frontend and checked semantic model, shared by native
compilation, compile-time execution, and public analysis libraries. Keep syntax,
checked programs, and native lowering as distinct boundaries; add another
representation only for a demonstrated consumer. LLVM 22 through the Rust
Inkwell binding is the current backend, not a source-language dependency.

Proofs compose through shared expression, control-flow and contract rules, with
separate arithmetic, sequence and storage theories. Extend those theories and
reuse verified contracts instead of enumerating application types or expression
combinations. Unsupported or exhausted reasoning remains unproved, never an axiom.
Compile-time SMT discharges unresolved supported obligations; keep cheap local
proofs in process and keep solver dependencies out of emitted applications.

Compile ordinary calls and scalar operations directly. Carry GC, fault, and
scheduler context only where required. Dependency semantics do not require an
executor or execution graph around every function. Library policy belongs in
Loom; the runtime supplies irreducible memory, scheduling, and platform operations.

Develop ordinary applications alongside the compiler. Each new capability must
work through real check/build/test/run and a representative source program.
Keep tests proportional to the changed boundary, without rebuilding a second
interpreter or a large dual-backend comparison framework.

## N0 — A native vertical slice

**Gate met for the documented subset on macOS, Linux, and Windows.**

- Directory packages, explicit imports, `pub`, colocated tests and test isolation.
- Functions, scalar and aggregate types, generic code, control flow and shared data.
- Source libraries for text, collections, errors and real file I/O.
- Constrained construction and mandatory postconditions within a sound bounded
  proof fragment; unsupported required proofs block the build.
- Real `loom check`, `build`, `test`, and `run`, including boundary failures,
  excluded library tests and straightforward scalar/record native code.

This is a usable foundation, not completion of the language or standard library.

## N1 — Move the compiler into Loom

**Self-hosting gate met. Delivery remains development-toolchain quality.**

Source loading, parsing, binding, typing, proofs, and checked-program emission
run as compiled Loom. Rust owns LLVM lowering, linking and private runtime/platform
boundaries, not a parallel source frontend. Public parser/project/analysis libraries
share the implementation without requiring the CLI or native backend.

Stages are bootstrap generations, not language versions:

1. Obtain a validated existing compiler. Cold Unix builds recover it from the
   pinned historical Rust seed and immutable Loom checkpoints; Windows uses a
   source-bound checked stage 0 reproduced on Unix CI.
2. Stage 0 builds stage 1; stage 1 builds stage 2; stage 2 builds stage 3.
3. Compare stage 2/3 artifacts and selected diagnostics, then run native compiler,
   source `std`, and application tests. Agreement is evidence, not correctness proof.

Daily development uses the single-stage `--dev` build. Compiler production
sources remain on a conservative seed subset: a new user feature alone does
not justify another checkpoint. Raise the minimum seed only for substantial
simplification or measured benefit, batching necessary changes.

CI stages and tests relocatable toolchain archives for all three hosts. Published
releases, bundled host LLVM/linker dependencies and additional target guarantees
need separate delivery evidence. The historical seed stays a recovery input in
Git history, not maintained legacy compatibility.

## N2 — Complete the language and source library

**In progress.** The current native path includes concepts/dyn, associated types,
compile-time programming, typed macros/reflection, bounded contracts, moving GC,
lexical cleanup and real stackless async I/O. These do not close the gates below.

| Workstream | Remaining exit criteria |
| --- | --- |
| Constraints and proofs | Broaden sound contract composition, entry-state reasoning and mutation preservation; close the sorting/permutation and constrained shared-data stories without replacing proof with tests. |
| Compile-time programming | Complete accepted pack/type/function combinations and useful reflection while retaining explicit requirements, selected branches, visibility and tracked inputs. |
| Source `std` and async | Complete application I/O beyond verified TLS streams, including [shared workers](docs/rfcs/tasks.md#shared-workers) and remaining socket policies; close shared GC, memory-safe access, explicit synchronization and structured drain together. |
| Programming feedback | Broaden checked editor operations and incremental coverage; reduce real edited-source latency and memory, not only unchanged-cache timings. |
| Effect reasoning | Validate transformations against actual data/control/resource conflicts, including aliasing, faults and cleanup; unknown overlap must retain ordering. |

The module version slice now resolves SemVer Git tags and normalizes compatible
requests, with source identity, explicit local/graph fork policy, and locked
offline builds across anonymous/authenticated sources. Version labels are not
behavioral compatibility proofs.

Stateful List length proofs now cover append, indexing and length preservation
through loops, with per-invocation `old(length(values))` observations. This is
groundwork for the sorting story below. Bounded indexed observations now cover
read/write relations, immutable entry-element snapshots and conservative alias
invalidation. Length-preserving loops can retain proved element bounds and
storage conservation relations through induction; this is not quantified
ordering, permutation or general heap reasoning. Bounded call-write analysis
preserves untouched observations across extracted helpers without assuming
unknown indices or handles are disjoint.

Use these complete acceptance stories, rather than a count of syntax features:

- An application combines private directory packages, colocated tests, constrained
  inputs, resources and async I/O through the native commands and editor.
- A fixed-shape shared view survives source resizing; content constraints cannot
  be invalidated through another alias. Explicit isolation remains explicit.
- Sorting proves its declared normal-return properties. Unknown proofs reject;
  no undeclared termination or non-aliasing promise is inferred.
- A source compiler uses generic algorithms, known callbacks, metaprogramming and
  tracked inputs without compiler-only shortcuts or a longer seed chain per feature.
- Published compiler data survives later passes and suspension; mutable drafts
  cannot silently invalidate another pass's constrained shared data.

Detailed current boundaries and runnable examples belong in the
[compiler guide](compiler/README.md) and [status](docs/project/implementation-status.md),
not repeated completion narratives here. Unsupported compiler cases must reject
or conservatively recheck as appropriate; they must not redefine the target.

## N3 — Deliver semantic change and deployment workflows

**Bounded prototypes exist; the general workflows remain open.**

Build on compiler identities, bindings, contracts, effects and immutable build
inputs. Reuse an existing version engine; keep ordinary files and editors, not a
second authoritative source store or mandatory AST editor.

Close the three [accepted stories](docs/rfcs/change-and-deployment.md):

1. Evolve libraries across versions/forks without hiding changed bindings or
   merging nominal identities merely because names match.
2. Merge moves and edits using explicit identity and the actual baseline;
   preserve unresolved intent and show stale feedback without replaying effects.
3. Compare the effective deployed basis with the candidate artifact; require a
   complete migration, predetermined failure policy and executable recovery.
   Preserve downgrade data and include it in later re-upgrade compatibility.

The current SQLite orders executor and single-package semantic merge trial are
evidence for narrow cases, not arbitrary-schema migration or general semantic
version control. Online coexistence, throttling, unknown outcomes and hotfix
compatibility require their own complete evidence.

## Delivery discipline

Use focused PRs, keep the three-platform native/bootstrap gates green, and remove
replaced paths. Documentation must separate goals, current capabilities and
limitations. No compatibility obligation exists for unpublished prototypes.

Measure fresh-process check/build/test compilation and memory on representative
growing packages. Separate source checking, LLVM, linking and cache conditions.
An unchanged hit is not an edit benchmark. Measure native scalar, record and
collection workloads too; inspect generated code before adding an optimization
layer. No performance target permits weaker contracts, sharing or cleanup.

Alternate backends, stable FFI/plugin ABI and cross-cutting/AOP composition are
separate future work. Reconciliation belongs in libraries and systems, not a
mandatory language operator runtime. These do not block the current self-hosting gate.
