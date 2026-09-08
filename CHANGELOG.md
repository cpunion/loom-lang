# Changelog

Loom has not published a release. This records changes to the active
implementation, not a compatibility ledger for previous prototypes.

## Unreleased

- Support named async function values and synchronous Task factories with the
  same structural callable type. Preserve one-shot transfers and indirect creation
  locations through native code pointers, including record/List storage and
  suspension. Ordinary callbacks keep their ABI; capturing closures and stored
  Task aggregates remain unfinished.

- Retain lexical cleanup across suspension, add worker-backed file Tasks, and
  support async methods through concrete, generic and sparse dynamic witnesses.

- Support direct Task parameters and returns, including generic forwarding and
  nested Task results. Async callees adopt argument subtrees; completed producers
  retain returned children until extraction. Check one-shot obligations at entry,
  returns and partially evaluated calls, without source ownership syntax or an
  executor for synchronous helpers. Task-bearing aggregates remain unfinished.

- Launch the VS Code programming trial directly with `npm run try` in
  `editors/vscode`, with format/check/build/test/run tasks and a feature-writing
  exercise. Demonstrate both same-directory test forms in the file-tool library.
  `loom run` now preserves application exit codes in the portable 0–255 range
  without appending a compiler error to a normal nonzero exit.

- Add `std.time.monotonic_ns`, `sleep_ns`, `sleep_ms` and `sleep_until_ns`.
  Loom timer states preserve a single evaluated deadline; runtime notifications
  requeue them through a lazily created reactor. Idle waits block without spinning
  or creating per-task threads. Relative delays begin when the task body runs;
  monotonic deadlines are process-local and scheduling promises no fairness.

- Add the first source Task slice: hot child creation, `async fn main`,
  `test async fn`, postfix `.await`/`.await?`, and one-shot local obligations.
  Loom lowers typed frames and resume functions for a single-threaded CPU ready
  queue, with fault propagation and cancellation of queued/suspended descendants.
  Socket adapters, general worker operations, joins and Task-bearing aggregates
  remain unfinished.

- Add a private native resume fault boundary: drain live lexical cleanups,
  restore GC roots, then unwind through LLVM frames into an owned diagnostic.
  Ordinary synchronous faults still terminate; Tasks use the boundary per resume.

- Add private owner-scoped frame roots over the existing moving collector,
  with dense scanning, generation-checked reuse and noncollecting handoff.
  Generated coroutine frames and suspended cleanup reuse these roots.

- Add a private portable wait ABI for timers, native readiness and worker
  completion, with one-shot delivery and generation-checked cancellation.
  Source timers and asynchronous file operations use this path.
  Final executable links discard unreferenced runtime sections; library objects
  keep their existing export/reachability policy.

- Add native and compile-time Int bitwise operations and checked shift counts,
  without additional runtime operations or changes to arithmetic overflow rules.

- Separate module/package identity from display names. Same-named path modules
  coexist with importer-local visibility, distinct nominal types and test roots.

- Add explicit dynamic associated-type bindings, exact boxing checks and generic
  substitution. Reuse native witness representation and sparse reachability.

- Add generic implementation headers with scoped prerequisite checking,
  associated-type substitution and native method/witness specialization.
  Reject overlapping templates and cyclic conformance requirements.

- Add concept default method bodies with explicit conformance, override selection,
  abstract checking and associated types. Reuse direct specialization and sparse
  dynamic witnesses without a new runtime dispatch mechanism.

- Add function-valued `comptime` parameters with source-identity specialization,
  generic forwarding and direct native calls. Preserve abstract requirements,
  runtime capture isolation, preconditions and compile-time effect rejection.

- Compile typed List/Text/Bytes accesses and buffer push fast paths directly.
  Finalize linked GC root frames after inlining, protect allocation-crossing
  snapshots, keep ordinary locals promotable, and reallocate private buffers.
  Remove obsolete runtime accessors; retain bounds, overflow and shared-data
  semantics. Record same-session runtime and self-check benchmark comparisons.

- Add static associated types and bounded generic records/enums. Normalize
  projections through explicit evidence, preserve generic declaration choices,
  and infer independent parameters before checking associated arguments.

- Add `loom test --no-run` and optional compiler-latency measurements for startup,
  isolated test compilation and generated package growth, retaining input hashes
  and separate native phase timings.

- Add native `dyn C` with statically established evidence, GC-owned receiver
  snapshots and sparse method tables. Reuse ordinary generic checking and call
  reachability; unused implementations and method slots are not emitted.

- Add source `Option[T]` and list endpoints, emptiness, shallow cloning, reversal
  and alias-safe append. Compile-time and native execution share the same library code.

- Add static nominal concepts, explicit implementations, generic bounds and
  conditional conformance queries. Lower selected methods as direct calls,
  preserve test isolation, and provide source `std.display` without a runtime registry.

- Add source Text search, prefix/suffix matching, Unicode trimming, split and
  byte-builder-backed join, including compile-time execution and GC stress tests.

- Execute Float in the shared compile-time evaluator and support Float-based
  constrained types, with direct constant construction and once-only dynamic
  checks. Bootstrap through a pinned Loom source checkpoint, leaving the frozen
  Rust frontend unchanged; required unsupported Float proofs still reject.

- Add native IEEE binary64 Float and explicit numeric conversions. Keep decimal
  syntax/error policy in Loom std over narrow numeric codecs; scalar arithmetic
  needs no Loom runtime. Establish the Float-capable bootstrap checkpoint before
  adopting Float in the compiler's evaluator.
- Reuse same-type GC temporary slots across completed statements and exclusive
  branches. Preserve live argument snapshots, local roots, and deterministic
  bootstrap without changing the runtime ABI or adding a liveness IR.
- Add structural tuples, positional access, and exactly-once `let`/`var`
  destructuring to the Loom-written frontend. Reuse aggregate native layout and
  compile-time graph reconstruction, including generic and shared fields.
- Add pure Loom decimal integer parsing with explicit syntax/range errors and
  compile-time execution. Exercise command-line parsing and constrained input
  in an ordinary application with colocated tests.
- Upgrade the native bridge and frozen source seed to LLVM 22 through the existing
  Inkwell 0.10 binding. Keep Rust 1.88 and avoid a second LLVM install for bootstrap.
- Separate the backend-neutral codegen interface from the LLVM implementation;
  bound LLVM scheduling search to avoid its large-block compile-time regression.
- Pass the macOS, Linux, and Windows LLVM 22 bootstrap and full native CI gates.
  Linux recovers its source seed independently; Windows builds native stages
  from the same workflow's checked export. Align the Windows CRT and SDK library
  paths and reduce recursive evaluator stack frames without lowering budgets.
- Protect source extensions and native/IR outputs against case aliases on
  case-insensitive filesystems.
- Implement the Windows MSVC native/runtime path, Unicode/binary I/O, canonical
  package paths, and executable suffixes. Add checked compiler export and
  same-workflow bootstrap transfer into the Windows CI gate.
- Allow pure helper calls in `Int` type predicates, validating their operation
  closure even for unused declarations. Fold known predicates without weakening
  runtime construction or required proofs. Extend `comptime if` type guards to
  generic and nested types.
- Reconstruct shared `comptime` List/Bytes results as fresh runtime graphs,
  preserving internal aliases and cycles. Reuse successful pure evaluations
  within one check without adding a persistent cache or mutable globals.
- Share checked types, concrete call targets, and source-snapshot checks through
  `std.loom.analysis`, exercised by an independent in-memory semantic consumer.
  Add bounded pure `comptime` evaluation and selected type-guard branches over
  the same checked model as native compilation; required proofs remain separate.
- Share manifest parsing, project loading, and declaration binding through
  `std.loom` libraries. An ordinary project inspector reports symbols, source
  positions, and visible name candidates without invoking the compiler;
  declaration binding remains distinct from typed analysis and persistent identities.
- Index package name lookup once, optimize the runtime without removing dev
  checks, and add single-stage O1 compiler rebuilding. Measure compiler latency,
  memory, and native phases with a small macOS benchmark harness; retain full
  O2 bootstrap verification.
- Expose the compiler's syntax implementation as ordinary `std.loom` source,
  lexer, AST, and parser packages. An independent Loom package uses them to
  inspect in-memory code and diagnostics without compiler or backend imports.
- Bootstrap the Loom-written package loader, binder, type checker, and bounded
  prover through successive native stages, with stage agreement and source
  compiler/`std` tests. Retire the active Rust source frontend; retain the
  LLVM/platform tool and a pinned historical bootstrap fallback.
- Parse the current syntax in Loom, including the frontend's own sources.
  Add recursive data through lists and postfix `Result` error propagation.
  Align surviving docs and issue templates with the accepted design and single
  native path; reject unsupported nested manifest fields instead of ignoring them.
- Begin N1 with a native Loom-written lexer, source positions, diagnostics,
  and file-based command-line frontend. Add source Unicode/UTF-8 helpers,
  integer formatting, and process/std-error APIs; replace the ASCII scanner.
- Complete the N0 native vertical slice with scalar constrained construction,
  check-free widening, and source-owned file/stdout write loops. Unknown
  construction returns a source `Result`; failed required proofs still reject.
- Compile generic records/enums and exhaustive matches with concrete layouts.
  Add shared lists, UTF-8 Text, a small nonmoving collector, and source `std`
  text/list/result/file packages, exercised by a native source scanner.
- Start a small Rust/Inkwell compiler with real native check/build/test/run,
  checked scalar arithmetic, control flow, local package imports, isolated
  tests, source `std.int`, and required static postconditions in a bounded
  proof fragment. See the [compiler guide](compiler/README.md).
- Remove the superseded compiler, interpreter, runtime, dedicated fixtures,
  benchmark/release infrastructure, and stale implementation documentation.
  The root workspace and macOS gate now serve the native compiler and its small runtime.
  The removed work remains recoverable in Git history.

The [roadmap](ROADMAP.md) tracks incomplete goals separately.
