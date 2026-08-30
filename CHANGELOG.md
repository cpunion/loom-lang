# Changelog

Loom has not published a release. This file summarizes the single current
unreleased implementation; it is not a migration ledger. Exact compiler-private
format and ABI identities live in
[Versioning and compatibility](docs/project/versioning.md), and implementation
coverage lives in [Implementation status](docs/project/implementation-status.md).

## [Unreleased]

### Initial implementation

- A conventional statically typed source language with Go-style `name Type`
  declarations, inferred `Unit` returns, final-expression returns, postfix
  `.await`, tuples, records, closed sums, pattern matching, loops, and explicit
  `discard`.
- Refinement types, record invariants, preconditions, postconditions, checked
  conversions, generic functions and records, associated types, concepts, and
  explicit `dyn C` values. Closed-world conformance planning permits direct
  dispatch, witness erasure, and dead-conformance elimination; Loom does not
  discover conformances by converting an untyped value to `dyn C` at runtime.
- Automatic moving-GC memory management without ownership, borrow, or lifetime
  syntax. Lexical `scoped` cleanup and block-scoped `defer` provide deterministic
  external-resource cleanup, including compiler-enforced `MustScope` resources.
- Stackless compiler-lowered coroutines, structured `Task[T]` trees, postfix
  await, fixed heterogeneous tuple joins, dynamic homogeneous List joins, and
  `all`, `any`, `settled`, and `race` policies.
- Real timer and I/O readiness integration on macOS, Linux, and Windows through
  a platform-neutral wait ABI. File and Socket values contain opaque monotonic
  capability tokens; concrete RAII owners stay in the active Task ledger.
- A complete frontend pipeline from source through syntax, HIR, semantic
  analysis, checked MIR, typed LCIR, and LLVM object generation. Entrypoint
  closure starts from the selected main, tests, or export and excludes
  unreachable functions and conformances.
- Exact typed LLVM representations for scalars, products, closed sums, managed
  Text, Bytes, Lists, TextMaps, dynamic concepts, Tasks, and typed async I/O.
  Resource close is a normal LCIR instruction, and typed I/O coroutine frames
  contain exact `Result` layouts rather than a universal runtime value.
- `loom check`, `build`, `test`, `run`, `fmt`, package, cache, runtime-bundle,
  and artifact workflows. Persistent inputs are bounded, version-exact, and
  rejected rather than upgraded when their current identity does not match.
- Compiler-distributed `std` source modules compiled through the ordinary
  frontend, including integer and JSON parsing, logging wrappers, process
  wrappers, resource concepts, the public `DecodeTextError` and `PathError`
  enums, and the complete public `std.log` graph. Logging now resolves through
  ordinary source `DefId` values and has no universal-value native fallback;
  only its exact-owner private typed write primitive remains compiler-owned.
  Runtime bundles version the checked-MIR `IoError` nominal-tag shift caused
  by removing the former synthetic error-type slots, so an older bundle cannot
  be accepted under the new MIR layout.
  `std.process.arguments` builds its List in source over typed snapshot
  primitives; process input has no universal-value or checked-MIR runtime path.
  JSON parsing has no compiler opcode or runtime entry point; canonical JSON
  formatting uses an exact typed layout boundary.
- A precise moving collector, lazy single-threaded executor, OS reactor, bounded
  blocking pool, structured cancellation, deterministic cleanup, and strict
  runtime-bundle identity checks.
- Cross-platform CI, release smoke gates, fuzz and conformance checks, and
  reproducible native benchmarks against C, C++, Go, and Rust.

### Current boundary

- The project accepts only its current source language, lockfile, library,
  artifact, cache, and runtime-bundle formats. There are no aliases, upgrade
  readers, dual-format writers, deprecated runtime shims, or compatibility
  promises for unpublished formats.
- Typed LCIR is the destination compiler path. A complete checked-MIR native
  route still handles artifacts containing genuine LCIR coverage gaps; the
  compiler never mixes both representations within one native artifact.
- `std.json.parse_json` is a source-backed, iterative, depth-bounded parser that
  lowers completely through typed LCIR. The controlled standard-library
  fixture has no native-route exception.
- Live programming, AST editing, AOP/advice, operator runtimes, runtime
  conformance discovery, ownership/borrow syntax, and a multithreaded executor
  are outside the current language.
