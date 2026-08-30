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
  File and Socket operations in both the recoverable `Result` family and the
  faulting family lower only through typed LCIR. Resource close is a normal
  LCIR instruction; generated I/O callbacks publish the exact direct result or
  record the operation fault without a universal runtime value.
- `loom check`, `build`, `test`, `run`, `fmt`, package, cache, runtime-bundle,
  and artifact workflows. Persistent inputs are bounded, version-exact, and
  rejected rather than upgraded when their current identity does not match.
- Compiler-distributed `std` source modules compiled through the ordinary
  frontend, including integer and JSON parsing, logging wrappers, process
  wrappers, resource concepts, the public `DecodeTextError` and `PathError`
  enums, and the complete public `std.log` graph. Logging now resolves through
  ordinary source `DefId` values and has no universal-value native fallback;
  only its exact-owner private typed write primitive remains compiler-owned.
  `IoErrorKind` is now an ordinary `std.io` source enum whose exact definition
  is made available through the prelude; its ten compiler builtin constructors
  and fixed MIR type slot are gone. `IoError` is likewise an ordinary protected
  source record with source-owned `kind` and `message` methods. Its exact
  standard-library identity receives the private typed-I/O representation;
  application source cannot construct, project, or compare it. `File` and
  `Socket` are likewise protected empty source records. Their public methods,
  `Dispose` implementations, and `MustScope` conformances have ordinary source
  identities; their exact canonical definitions alone receive hidden one-Int
  MIR capability storage. Every scoped cleanup now follows the selected source
  `Dispose.dispose` witness, whose body reaches the private close leaf and typed
  LCIR `ResourceClose`; MIR has no File/Socket-specific scoped action. All
  public `std.file` and `std.net` functions resolve through ordinary source
  wrappers, and the Path forms reuse their Text counterparts. Only 16
  exact-owner resource/I/O/close leaves plus two protected error access leaves
  remain below source. Interpreted MIR artifact 39 and persistent cache schema
  15 reject the removed semantic types, fixed slots, and special cleanup tags
  instead of decoding them through compatibility paths.
  Native runtime ABI 36 (`runtime-v30`) records the removal of the former
  universal File, Socket, and close entry points and their fixed File/Socket
  nominal IDs; the complete identity also retains the earlier removal of
  universal logging. The 16-byte `typed-io-v1` outcome uses its primitive
  payload for either a resource token or a closed fault class, preserving
  `InvalidPort`, `SocketResolveFault`, and operation-specific host faults; the
  `typed-resource-v1` close boundary remains unchanged.
  `std.process.arguments` builds its List in source over typed snapshot
  primitives; process input has no universal-value or checked-MIR runtime path.
  JSON parsing has no compiler opcode or runtime entry point; canonical JSON
  formatting uses an exact typed layout boundary.
- A precise moving collector, lazy single-threaded executor, OS reactor, bounded
  blocking pool, structured cancellation, deterministic cleanup, and strict
  runtime-bundle identity checks.
- A dedicated typed-I/O fixture closes real `check`, `build`, `test`, and `run`
  commands and rejects universal I/O symbols in its object. The integrated
  standard-library native test now uses the production route and exercises real
  filesystem and loopback-socket I/O.
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
- Reachable File or Socket I/O is an LCIR-only boundary. Direct checked-MIR
  emission rejects it, and `Automatic` preparation fails closed instead of
  selecting checked-MIR fallback when another reachable feature prevents a
  complete LCIR artifact. Unreachable I/O does not affect route selection.
- `std.json.parse_json` is a source-backed, iterative, depth-bounded parser that
  lowers completely through typed LCIR. The controlled standard-library
  fixture has no native-route exception.
- Live programming, AST editing, AOP/advice, operator runtimes, runtime
  conformance discovery, ownership/borrow syntax, and a multithreaded executor
  are outside the current language.
