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

No historical release notes have been reconstructed. Future entries should
describe observable changes, not reproduce the Git commit log.
