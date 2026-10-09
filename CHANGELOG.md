# Changelog

Loom has not published a release. This records changes to the active
implementation, not a compatibility ledger for previous prototypes.

## Unreleased

- Keep nested pure helper results relative to their enclosing evaluation path,
  while retaining complete guards on every partial-operation check. Total
  parents no longer repeat their children's fault obligations. Nested enum
  transfer contracts compose within the existing proof budgets; eager argument
  faults, selected payload safety and loop induction remain checked.

- Propose enum tag and guarded immutable-payload prefix relations through the
  existing entry/all-backedge loop checks. Consume-and-build loops can prove
  reversed enum contents, including inline payload fields beside NaN and shared
  children. Pure Boolean helpers compose exhaustive return paths in a compact
  logical form without dropping eager checks or increasing proof budgets.

- Extend logical List columns to nominal enum tags and guarded payloads,
  including nested enum/record refinements and pure constraint helpers.
  Reads, writes, immutable entry elements and typed Task results reuse the
  existing array algebra; inactive payloads supply no unconditional facts and
  mutable children are not frozen. Preserve untouched columns when storing a
  payload-free variant, without runtime layout changes or a larger proof budget.

- Compose pure enum match predicates in required contracts through checked
  nominal tags and guarded payload projections. Nested/guarded patterns,
  correlated conditional receivers,
  helper requirements, eager argument faults and selected payload invariants
  retain the ordinary proof rules; no runtime reflection or library axiom is added.

- Retain checked unreachable-allocation facts for opaque inputs and completions,
  including incompletely observed List graphs. Mutable edge stores widen
  possible ancestor graphs. Loop entry/backedge
  checks withdraw failed frames and recheck dependent returns; publication
  revokes only affected headers and never creates a false loop hypothesis.
  Verify both source `std.resource.generate` length contracts without a factory
  axiom, ownership syntax, or runtime proof metadata. Contracted synchronous
  and async helpers share optional private-return origin inspection without
  leaking body value facts through their declared abstraction.

- Allow pure postcondition helpers to observe synchronous MustScope results
  through their logical borrowed binding. Ordinary executable calls still
  require `scoped`; freshness, cleanup and required proofs are unchanged.
  Nested proof helper frames also remap resource-drain locals, independently
  of cleanup registration IDs.

- Reduce temporary allocations in exact proof arithmetic. Reuse canonical
  read-only digits and linear terms for normalization, zero and unit operations;
  range checks recognize unit coefficients without constructing constants.
  Arbitrary precision and all proof obligations are unchanged.

- Declare and verify dynamic `std.task.all`/`settled` result lengths through
  ordinary source contracts, including resource payloads. Loop analysis follows
  tagged cleanup bodies instead of rejecting their metadata; disarmed cleanup
  skips no reachable effects and unsupported live drains still reject.
  Retain every completed helper's call-free guarantee within the existing
  check snapshot, only after its entire contract group passes. Typed one-shot
  result headers avoid redundant optional body-origin inspection.

- Avoid repeated quadratic structural contradiction searches already covered by
  integer ranges, and skip wrapped-complement searches before any negated fact.
  Retain nonlinear, compound and explicit-negation checks without adding proof
  assumptions, persistent state or solver queries.

- Infer retained immutable List columns against captured entry versions through
  the existing quantified loop fixed point. Consume-and-build loops can prove
  reversed contents without auxiliary source contracts or counters, including
  inline fields and early returns. Shared children stay opaque, Float equality
  keeps IEEE semantics, and every proposal needs entry/backedge verification.

- Unify loop affine observations over scalar/inline values and List extents.
  Prove consume-and-build length conservation without an auxiliary counter,
  including rebound inline fields and unchanged local handles. Every equation
  needs entry and all-backedge verification; rebinding, alias interference and
  cleanup cannot retain an invalid relation. No runtime invariant is added.

- Compose logical List reads with checked scalar and inline element refinements.
  Preserve representation conversions in pure helper expansion and import type
  invariants only over each version's valid indices. Dynamic Task joins and
  entry-value scans reuse these rules; bounds remain separate obligations and
  mutable child graphs stay opaque.

- Frame unreachable private List storage across known async inputs and unknown
  callback arguments. Follow evaluated inline values, selected enum payloads
  and completely observed nested Lists; possible aliases still publish together.
  Opaque graphs, missing/evicted cells and exhausted traversal retain conservative
  invalidation. Source stateful generation length contracts reuse these rules.

- Retain proved private result headers through contracted async abstractions.
  Optional bounded body inspection projects only storage provenance: declared
  contracts still supply all value/content facts, and returned fields gain no
  alias separation. Shared, published or unknown origins remain unframed;
  changed producer origins invalidate warmed frontend-cache proofs.

- Preserve the sole transferred Task-bearing List header's child-entry state
  through known async completion, including inline fields and enum payloads.
  Release caller heap facts at hot Task creation; unknown completion cannot
  restore those snapshots. Source dynamic `all`/`settled` length proofs reuse
  ordinary calls and loops, without a join axiom or runtime copying.

- Infer finite no-result source helper effects through the same checked body
  frames as async completion, including List updates and lexical cleanup.
  Unsupported optional inference retains conservative summaries. Task drain
  invalidates published aliases without discarding unpublished storage.

- Retain proved unpublished List storage across suspension and closed-input
  call summaries. Track conservative publication through Task inputs, shared
  stores, function captures, dyn boxing and unknown callbacks in both build
  modes; published aliases gain no frame. Prove source List construction across
  real waits and reject a cached edit that transfers its output into a Task.

- Infer stable List header identities through loop entry/backedge verification,
  including inline aggregates. Rebuild the abstract head when an identity
  hypothesis fails and retain publication loss across passes. Compose generic
  consume-and-build and dynamic Task batch length contracts without assuming
  alias separation or retaining stale elements.

- Prove bounded length/counter conservation through rebound Lists and inline
  projections. Reuse entry/backedge induction for generic consume-and-count
  loops without assuming header identity or alias separation; recheck changed
  counter updates after frontend-cache reuse.

- Allow erased postconditions to observe Task-bearing List metadata through
  pure source helpers, including `old` and nested fields. Keep executable
  helpers, preconditions, cleanup and dynamic slots under ordinary one-shot
  Task checks; logical-only instances never enter native reachability.

- Keep independent immutable Task outcomes as tagged proof values with guarded
  producer facts, rather than eagerly enumerating the terminal-status product.
  Wide source `settled` tuples now retain normal guarantees within the same
  bounded verifier; mutable and multi-return results remain conservative.

- Compose Task outcome and terminal extraction guarantees through the checked
  terminal schema. Only completed values inherit producer facts; faults and
  cancellation retain independent paths. Source tuple `settled` reuses these
  rules, including edited-producer cache invalidation.

- Infer normal completion facts from bounded known async source bodies through
  the existing symbolic executor. Compose tuple `all` through its source policy
  and checked registration/drain/fault boundaries, without a join axiom. Keep
  declared abstractions, unknown callbacks and suspension heap invalidation.

- Reuse List extent/content rules for transfer `append`, `take_last` and
  `replace`, including displaced values, shared-header updates and extracted
  Task promises. Optional case names/order follow the checked schema; writes,
  suspension and unknown task targets supply no stale content guarantees.

- Compose required function guarantees through checked enum construction and
  matching, including generic/nested patterns, guards, dynamic calls and Task
  payloads. Reuse ordinary branch and loop rules, preserving input effects and
  invalidating shared storage across writes or suspension.

- Isolate automatic `run`/`test` binaries in exclusive per-invocation directories
  and clean them after child exit or compilation failure. Concurrent commands
  no longer overwrite each other or a retained build; `test --no-run` keeps its
  explicit/default artifact behavior.

- Preserve verified async completion guarantees through finite synchronous Task
  factories, saved results, branches and supported loops. Reuse the existing
  symbolic call frame with once-evaluated inputs and lexical cleanup; bound call
  depth and reject unsupported recursive bodies without overflowing the stack.

- Compose required variadic contracts across checked synchronous family calls,
  retaining symbolic widths, declared element bounds and ordered type schemas.
  Forward named value/tuple sequences without sampling types or arities; keep
  helper proof calls outside native reachability and recheck changed bindings.

- Extend universal variadic contract induction to structural tuple inputs with
  fixed fields, repeated/independent packs and elementwise expansions. Reuse
  affine extents and scalar/List loop proofs; keep mixed element observations
  opaque and native iteration statically expanded.

- Infer checked entry-difference relations between written Int loop cursors and
  counters. Reuse ordinary entry/backedge verification for separate updates,
  symbolic offsets and stronger structural-pack bounds; retain source overflow
  obligations and the existing inference budget.

- Reuse mathematical affine forms for loop relations and suggest weighted
  counter conservation from constant translations. Prove different-rate and
  opposite-direction counters through the existing induction checks; update
  syntax supplies no trusted facts or runtime checks.

- Allow checked scoped-resource borrows through directly awaited async calls;
  reject escaping borrowing Tasks and keep NoSuspend/Task-result restrictions.
  Add source Stream factory pipelines with ordinary nested cleanup, cancellation
  drain and a real file-line trial. No ownership syntax or runtime bridge added.

- Remove shared-access guards for proven invocation-local storage, retaining
  moving-GC roots and cancellation checkpoints. Preserve private List facts and
  prove fresh-result contracts in worker builds; publication and loop backedges
  conservatively invalidate privacy. No new source ownership syntax or seed.

- Add source `std.task.worker.run` with typed callback frames, shared moving GC,
  memory-safe mutable accesses, completion notifications and structured
  cancellation/fault cleanup. Keep no-worker lowering direct and revalidate
  required proofs for interference. Broader shared-state proofs and
  interprocedural escape summaries remain open.

- Add source `std.sync.mutex` with lexical `MustScope`/`NoSuspend` guards,
  normal/fault release and rejected same-thread reentrance. Guards cannot be
  copied, reconstructed from fields or held across await.

- Add a native worker cancellation boundary with wakeable mutex acquisition,
  live-root lexical drain and distinct cancellation/fault outcomes. Nested
  diagnostic catchers cannot swallow cancellation; mandatory cleanup completes
  before drain returns.

- Add compile-time `std.build.target` queries from the actual backend. Bind
  observed properties to checked artifacts, cache reuse and analysis; reject
  mismatched emission targets without adding runtime platform queries.

- Add explicit build options to CLI, source `std.build.option`, and editor
  queries. Bind the detached option map to frontend reuse, analysis freshness
  and v3 build receipts, without ambient environment reads or runtime operations.

- Add raw multiline Text with triple quotes, closing-indent removal and longer
  delimiters for embedded quotes. Preserve contents through formatting and use
  readable multiline assertion fixtures. No new runtime type or bootstrap stage.

- Add Boolean match guards with pattern bindings, source-order effects, scalar
  refinement facts and conditional Task transfer. Guards do not provide coverage;
  guarded MustScope matches reject. Reuse ordinary typed branches and async frames.

- Require newlines or `;` between record field declarations, rejecting fields
  separated only by spaces or commas. `loom fmt` emits one field per line and
  removes field semicolons while preserving comments. Statement syntax is
  unchanged; semicolons are not statement terminators.

- Add source `std.list.nonempty.NonEmpty[T]` with an explicit outer-copy
  conversion and a private invariant-preserving API. This is not an implicit
  constrained conversion of shared Lists.

- Prove required contracts over scalar fields of nested records and Int-backed
  constrained values; support `old(expr)` for immutable entry-state scalars;
  enforce inherited concept method contracts for default, override, static and
  dynamic calls. Shared mutable List predicates and snapshots remain outside
  this proof fragment.

- Permit explicit `impl D for dyn C` adapters, including associated bindings,
  without discovering a hidden concrete type at runtime. The current adapter
  boxes the known erased value; it is not a zero-allocation upcast.

- Join two heterogeneous Tasks as a tuple and clean up multi-field `scoped`
  resource aggregates, including fault exits. Dynamic homogeneous List joins
  remain available; general heterogeneous packs and resource Lists are not yet
  supported. Nested record resources now receive pending cleanup.

- Preview stable-ID source merges across ordinary files, including move plus
  independent edit. The read-only Git entry point reads three commits and
  rechecks the merged package; it does not apply or commit the result.

- Add deployment-basis and conflict analysis, an append-only SQLite event
  ledger, and a bounded offline orders migration with data-preserving downgrade
  and re-upgrade. `loom build --receipt` now binds the fixed executor's target
  to a real checked build and artifact digest. This is not a general migration
  engine, a signed receipt, or proof of the operator-declared storage mapping.

- Complete qualified package paths from existing binding spellings, retaining
  overloads, local receiver precedence, private/test scopes and distinct source
  instances. Filter type/constructor and dyn paths without guessing instantiations.
  Import-path discovery and automatic imports remain open.

- Add receiver-type member completion through shared Loom signature, local and
  match-scope checking. Offer visible fields, tuple indices, explicit concept
  methods and async Task `.await`, retaining generic/test visibility and full-token
  replacement. Reuse cursor recovery without turning hints into proof evidence.

- Recover missing cursor names/values and unmatched EOF delimiters in virtual
  completion snapshots, only after ordinary loading fails. Keep normal
  parsing/builds strict, retain original diagnostics and map insertion ranges
  back to the untouched user buffer.

- Keep spaces before grouped operands after binary operators, assignment,
  commas, match arrows and contract keywords in `loom fmt`, while retaining
  attached calls and unary prefixes. Preserve parsed structure and idempotence.

- Add compiler-backed name completion to VS Code and public
  `std.loom.analysis.complete_names`. Reuse package visibility and lexical
  scopes, preserve overload signatures and whole-identifier replacement, and
  accept body type errors without running proofs.

- Implement List-based `std.task.all/settled/any/race` in Loom, using one-time
  notification registration and indexed slot transfer. Drain losing subtrees
  before returning, including returned Tasks; preserve primary faults and report
  cleanup failures. Support inferred no-result generic payloads without exposing
  Unit syntax. Add native/compile-time `std.list.transfer.replace`. Tuple joins
  remain open.

- Add source `std.task.outcome` and `cancel`, with typed Completed/Faulted/Cancelled
  results, including no-result and Task-valued payloads. Preserve ordinary
  Result errors as successful values. Cancellation drains descendants, OS work
  and cleanup before returning; already-terminal outcomes remain authoritative.
  Fault diagnostics become owned Text. Public multi-task joins remain unfinished.

- Add private multi-task completion observation through typed Loom frames.
  Register children once, retain terminal order even for late registration,
  and detach one notification before ordinary typed await. Cancellation removes
  observations before parent cleanup. Public join policies remain unfinished.

- Transfer Task-bearing Lists, including recursive enum/List payloads. Add
  source `std.list.transfer.append/take_last`, retaining ordinary shared-header
  semantics and compile-time removal. Typed visitor functions adopt/retain each
  child without a new runtime ABI. Merge Task consumption across actual loop
  exits; a non-returning `while true` needs no unreachable return. Joins remain
  separate work requiring multi-task completion observation.

- Support Task-bearing enum payloads through ordinary matching, Option/Result
  and error propagation. Transfer only active-variant children using typed
  matches; empty variants create no Tasks. Reject wildcard drops and duplicate
  consumption, and preserve resource/purity rules without new runtime ABI.

- Support Task fields in tuples and records, with independent field consumption,
  whole-value transfer, tuple destructuring and multiple returned subtrees.
  Reuse typed native projections and the existing child set.

- Support named async function values and synchronous Task factories with the
  same structural callable type. Preserve one-shot transfers and indirect creation
  locations through native code pointers, including record/List storage and
  suspension. Ordinary callbacks keep their ABI; capturing closures remain open.

- Retain lexical cleanup across suspension, add worker-backed file Tasks, and
  support async methods through concrete, generic and sparse dynamic witnesses.

- Support direct Task parameters and returns, including generic forwarding and
  nested Task results. Async callees adopt argument subtrees; completed producers
  retain returned children until extraction. Check one-shot obligations at entry,
  returns and partially evaluated calls, without source ownership syntax or an
  executor for synchronous helpers.

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
