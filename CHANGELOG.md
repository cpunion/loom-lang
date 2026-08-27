# Changelog

All notable user-visible changes to Loom will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Loom is experimental and does not yet promise stable source, standard-library,
artifact, or runtime compatibility.

## [Unreleased]

### Added

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

### Changed

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
  executor; allocating or derived text and text nested in aggregates still
  select atomic whole-artifact fallback. This advances the LCIR dump to 7,
  artifact identity to schema 8, native-object domain to v4, and CLI object
  cache to v9 without changing the native runtime ABI.
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
  object cache to v11. Source contracts and assertions still select atomic
  fallback until their control-flow and cleanup lowering is complete.
- Interpreted MIR version 20 permits projected moves. They return the selected
  leaf and consume the complete aggregate root, preserving a simple initialized
  or moved local state without partial-initialization compatibility.

No historical release notes have been reconstructed. Future entries should
describe observable changes, not reproduce the Git commit log.
