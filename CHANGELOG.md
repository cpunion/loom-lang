# Changelog

All notable user-visible changes to Loom will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Loom is experimental and does not yet promise stable source, standard-library,
artifact, or runtime compatibility.

## [Unreleased]

### Added

- `NativeRoutePolicy::LcirOnly`, which turns incomplete whole-artifact LCIR
  coverage into a structured `NativePreparationUnsupportedLcir` error and
  retains the complete deterministic `SupportReport` for tooling and gates.
- An English documentation structure for installation, first use, core
  language features, packages, contribution, security, and project direction.
- A pull-request-only contribution policy and private security-reporting path.
- Verified Linux and macOS native CI coverage, plus a complete Windows native
  job whose support claim remains gated on successful runner evidence.
- A Windows x86-64 release-matrix entry that stages the native `.exe` tools and
  runtime `.lib`, runs release smoke gates, and produces a checksummed `.zip`;
  availability remains gated on successful Windows runner evidence.
- Trusted Linux and macOS base-versus-candidate benchmark comments with one
  exact comparison table and visible runtime-index charts.
- Explicit, checksummed runtime bundles for native linking, including strict
  target, ABI, archive, and linker validation.
- A side-by-side typed moving-heap ABI with exact fixed-pointer descriptors,
  an independent direct-pointer shadow stack, strict shared root limits, and
  forced-relocation evidence without constructing an executor.
- A versioned `typed-repeated-v1` allocator for tagless monomorphized
  containers. The collector copies bounded header and per-element pointer
  maps, owns capacity outside object bytes, and precisely rewrites repeated
  managed cells. This advances the collector identity to `gc-v9` and native
  runtime ABI to component 13 with `runtime-v7` while retaining the fixed
  `typed-gc-v1` wire.
- A typed `Text.get` runtime boundary which stages one Unicode scalar before
  its allocation safepoint, returns missing indices without allocating, and
  publishes a direct managed Text pointer without a universal `Value`. This
  advances Text identity to `text-v3` and native runtime ABI to component 14
  with `runtime-v8`; the collector remains `gc-v9`.

### Changed

- Typed LCIR now monomorphizes fully concrete generic records, invariant
  records, and refined wrappers into their exact direct product or transparent
  representations. Generic field projection, contract evaluation, calls,
  returns, and managed `Text` leaves remain typed SSA; compiler-private
  `TextMap[V]` stays opaque. The new `lcir-generic-products` fixture closes real
  `check/build/test/run`, host execution, legacy differential, and Linux/MSVC
  object evidence without a universal value or executor. Existing LCIR,
  artifact, object, cache, and runtime ABI versions are unchanged because this
  extends accepted source coverage without changing their formats.
- Typed LCIR now places source contracts at explicit checked boundaries.
  Closed-world calls evaluate every argument before checking `requires` with
  exact call-expression blame, then enter an assumed body whose receiver
  invariant is checked at entry. Normal tails and explicit returns run lexical
  cleanup before checking the current receiver invariant and `ensures`, while
  checked contract arithmetic retains its ordinary runtime faults. Typed
  `old` snapshots, managed Text/product/sum values, exhaustive contract matches,
  and concrete static-concept calls remain direct SSA without a universal value
  or executor. This advances the LCIR dump to 16, artifact identity to schema
  17, native-object domain to v13, and CLI object cache to v18 without changing
  native runtime ABI component 14.
- The controlled Core 0.1 evidence now requires its exact main graph to select
  typed LCIR. Its negative test graph keeps a named legacy allowance only for
  deliberate runtime-checked constrained-value and invariant construction.
- The controlled quality runner now compiles every native scenario through the
  same prepared Automatic route used by production commands. Its version 2
  evidence records the expected and actual route for each object, requires the
  typed fixture to remain LCIR, and permits legacy only through reviewed named
  allowances for fixtures whose missing typed coverage is explicit. This is an
  evidence-schema change only; LCIR artifacts, object domains, caches, and the
  native runtime ABI are unchanged.
- Typed LCIR now lowers `Text.get(Int)` directly to a collecting scalar-
  selection instruction returning the canonical managed `Option[Text]` sum.
  Missing or negative indices do not allocate; successful Unicode-scalar
  selection uses the typed runtime boundary, exact live-after roots, and a
  fail-closed status guard without a universal value or executor. This
  advances the LCIR dump to 15, artifact identity to schema 16, native-object
  domain to v12, and CLI object cache to v17; native runtime ABI component 14,
  `text-v3`, `runtime-v8`, and `gc-v9` are unchanged.
- Checked MIR now carries source-module provenance and a versioned
  compiler-known identity for the canonical `standard.resource.MustScope`
  marker. Artifact and cache validation independently require that qualified
  concept, its identity tag, the prelude id, and the empty marker shape to
  agree, while same-named concepts in other modules remain ordinary. This
  advances interpreted MIR to version 22 and the persistent compiler cache to
  schema 3 without changing source resource or cleanup semantics, LCIR, the
  native ABI, or the runtime ABI.
- Typed LCIR now keeps closed unboxed sums direct when a variant contains
  managed `Text`, including through nested products and sums. Each live sum
  receives deterministic candidate leaf cells; tag guards publish only the
  active variant and clear every inactive candidate, while post-GC reload
  rebuilds only the active payload from zero-initialized carrier bytes. This
  advances the LCIR dump to 14, artifact identity to schema 15, native-object
  domain to v11, and CLI object cache to v16. It reuses typed-shadow-stack v1
  and does not change native runtime ABI component 13 or `gc-v9`.
- Typed LCIR now expands source assertions, `defer`, and `scoped` disposal into
  direct lexical control flow. Cleanup registers only after its statement or
  scoped initializer succeeds, runs in strict LIFO order on normal block exit,
  return, and fault, preserves the first fault while suppressing later cleanup
  faults, and treats each branch body as an independent scope. Static-concept
  disposal is a monomorphic typed call with functional writeback; File and
  Socket disposal uses a new `typed-resource-v1` close ABI without a universal
  value, runtime cleanup stack, or synchronous executor. This advances the
  LCIR dump to 13, artifact identity to schema 14, native-object domain to v10,
  CLI object cache to v15, and native runtime ABI to component 12 with
  `runtime-v6`.
- Checked MIR now treats exhaustive pattern matching as an affine
  decomposition boundary for resource-bearing carriers. This restores
  `scoped resource = fallible_task.await?` and explicit `Result[File, E]` or
  `Result[Socket, E]` handling while still rejecting wildcard loss,
  unconsumed payloads, projected transfers, and matching an active scoped
  resource.
- The controlled release-quality runner now uses independent loopback I/O
  fixtures for interpreter and native execution, so one backend cannot retain
  a connection or consume the other backend's expected server slot. Native
  failures also retain structured fault metadata, and the Core 0.3 fixture
  digest is synchronized with its reviewed source.
- Failed native links now retain bounded diagnostics written to both standard
  output and standard error. This preserves MSVC `LINK` errors that Clang
  otherwise followed with only a generic failure summary.
- Windows host codegen now uses LLVM's deterministic generic x86-64 policy
  instead of entering LLVM 19's host-feature probe. This
  also aligns compiler and runtime-bundle CPU policy during `runtime pack`.
- Windows x86-64 `runtime pack` now obtains its canonical compiler-target
  identity without constructing an otherwise unused LLVM target machine; the
  embedded LLVM 19 data layout is checked by a cross-target regression test.
- Native object emission now initializes LLVM through the native-target entry
  point. Explicit targets select only their architecture initializer when the
  linked LLVM distribution is partial, so unrelated packaged backends cannot
  affect host object emission.
- The Windows native gate now isolates target-machine construction from first
  object emission and emits bounded, non-sensitive LLVM stage markers, so a
  process-level LLVM failure identifies its exact compiler boundary.
- Windows compiler binaries now link the official LLVM 19 `LLVM-C.dll`
  import library instead of LLVM's static MSVC component closure. This removes
  the private static `libxml2s.lib` rebuild, keeps LLVM allocation and target
  APIs behind one runtime boundary, and packages the required DLL beside the
  release compiler.
- Windows native linking now closes the writable construction handle for its
  private runtime-archive snapshot before invoking Clang/MSVC. A retained
  read-only, read-shared handle keeps the snapshot immutable and identifiable
  without preventing `link.exe` from opening the `.lib` input.
- Native `build`, `run`, `test`, and `debug` no longer obtain a runtime archive
  from the compiler build. Source builds must create a portable runtime archive
  and pack it beside `loomc`, or select a validated bundle explicitly.
- Interpreted MIR version 19 now treats source construction proofs as
  process-local: `.loomi` and nested `.loomlib` payloads replay their embedded
  predicate or invariant, while proof-bearing persistent compiler-cache layers
  rebuild from source to preserve cold/warm route and optimization behavior.
- Fresh-source proven refinements and record invariants use zero-check typed
  LCIR representations. Serialized proof rechecks atomically select the legacy
  route and retain canonical `ArtifactProofRejected` behavior.
- Reachable direct generic calls now form one bounded, deterministic LCIR
  instance closure with exact type and static-witness identity. Supported
  instances use direct typed LLVM signatures; nonregular or over-budget
  expansion selects whole-artifact fallback before LCIR construction.
- Eligible closed enums and exhaustive matches now use bounded typed LCIR
  lowering with direct native sum layouts. Float patterns follow IEEE ordered
  equality, including equal signed zeros and non-matching NaNs.
- Typed LCIR now lowers bounded nested record places through exact product SSA.
  Projected mutable receivers reconstruct the latest aggregate root on normal
  and fault edges without universal values, proxy storage, or runtime helpers.
- Typed LCIR now carries literal-proven `Text` values as one pointer to an
  immortal compiler-emitted object on 64-bit targets. Direct `length`,
  `contains`, and content equality require no universal value, GC root, or
  executor. This initially advanced the LCIR dump to 7, artifact identity to
  schema 8, native-object domain to v4, and CLI object cache to v9 without
  changing the native runtime ABI.
- Typed LCIR function effects now use one explicit transitive capability set:
  `MAY_FAULT`, `NEEDS_RUNTIME`, `MAY_COLLECT`, `NEEDS_EXECUTOR`, and
  `MAY_SUSPEND`. Independent validation recomputes the least call-graph fixed
  point and rejects both missing and invented capabilities. This advances the
  LCIR dump to 8, artifact identity to schema 9, native-object domain to v5,
  and CLI object cache to v10; it does not add runtime, GC, executor, or
  suspension operations.
- Typed LCIR fault origins now distinguish integer runtime faults from exact
  `AssertionFault`, `PreconditionFault`, `PostconditionFault`, and
  `InvariantFault` metadata. Contract metadata carries bounded user code,
  canonical messages, and concrete contract/blame spans through validation,
  dumps, identity, and LLVM machine diagnostics. This advances the LCIR dump
  to 9, artifact identity to schema 10, native-object domain to v6, and CLI
  object cache to v11. At that stage, source contracts and assertions still
  selected atomic fallback pending their control-flow and cleanup lowering.
- Bounded concrete static concept calls now resolve to ordinary direct LCIR
  calls. Conformance head arguments, conditional prerequisite proofs, and
  method proofs remain part of exact instance identity, while associated-type
  projections normalize to their concrete binding before representation
  planning. No runtime witness, indirect call, universal value, GC, or executor
  surface is added. Open instance keys are rejected independently by the
  builder and validator. This advances the LCIR dump to 10, artifact identity
  to schema 11, LCIR native-object domain to v7, and CLI object cache to v12.
- Dynamic `Text.concat` and Text-bearing tuples/records now compile through
  typed LCIR. One artifact-wide direct pointer representation covers literals
  and moving results whenever concat or a Text-bearing product is reachable; the
  runtime stages both complete UTF-8 inputs before collection, initializes a
  pointer-free typed leaf, and publishes it last. Exact live-after SSA root
  maps expand an unboxed tuple/record to deterministic managed-leaf cells in
  the typed shadow stack, rebuild aggregate uses from relocated aliases, omit
  dead edge arguments and empty frames, and construct no universal value or
  executor. Products remain exact SSA structs rather than heap objects.
  OOM remains an uncatchable process fault and invalid helper status fails
  closed. `Text.get`, Text inside enums or transparent/refined carriers, and
  managed lists remain atomic fallback. This advances the LCIR dump to 12,
  artifact identity to schema 13, native-object domain to v9, and CLI object
  cache to v14. Product leaf rooting reuses typed-shadow-stack v1 and did not
  change native runtime ABI component 11 or its `runtime-v5` identity.
- Interpreted MIR version 20 permits projected moves. They return the selected
  leaf and consume the complete aggregate root, preserving a simple initialized
  or moved local state without partial-initialization compatibility.
- The native runtime ABI component advances to `10`. Its exact identity uses
  `text-v2` and `runtime-v4`, keeps `gc-v8`, and includes `typed-gc-v1` plus
  `typed-shadow-stack-v1`. Existing legacy GC symbols and behavior remain
  available within the new whole-toolchain ABI identity.

No historical release notes have been reconstructed. Future entries should
describe observable changes, not reproduce the Git commit log.
