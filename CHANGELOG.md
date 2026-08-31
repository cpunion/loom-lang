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
  `discard`. Direct and mutual by-value nominal cycles are rejected instead of
  receiving an implicit box; recursion crosses an explicit indirect type.
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
  unreachable functions and conformances. Non-regular generic recursion,
  concrete-instance planning or substitution-growth exhaustion, and
  inconsistent checked generic metadata are compile failures rather than
  reasons to select the universal-value backend.
- Exact typed LLVM representations for scalars, products, closed sums, managed
  Text, Bytes, Lists, TextMaps, dynamic concepts, Tasks, and typed async I/O.
  Structural equality compares closed-sum tags once and enters one matching
  paired-payload case, so wide enums generate linear LCIR and LLVM control flow
  instead of a Cartesian pair of exhaustive switches. Functional `mut self`
  writeback retains the exact direct representation for scalars, products, and
  task-free closed sums, including `Option` and `Result`, on normal and fault
  exits.
  File and Socket operations in both the recoverable `Result` family and the
  faulting family lower only through typed LCIR. Resource close is a normal
  LCIR instruction; generated I/O callbacks publish the exact direct result or
  record the operation fault without a universal runtime value.
- `loom check`, `build`, `test`, `run`, `fmt`, package, cache, runtime-bundle,
  and artifact workflows. Persistent inputs are bounded, version-exact, and
  rejected rather than upgraded when their current identity does not match.
  Normal interpreter execution has no arbitrary instruction cutoff and matches
  native run duration; Rust embedders can select an explicit fuel and call-depth
  budget for untrusted or future compile-time evaluation.
  Ordinary source files may contain `test fn`, while `*_test.loom` files may
  also contain test-only helpers. `loom test` places both forms in a
  compiler-owned companion package with one-way access to the production
  package's private declarations; production builds and portable libraries
  exclude the companion completely.
- Compiler-distributed `std` source modules compiled through the ordinary
  frontend, including integer parsing, JSON parsing and formatting, logging wrappers, process
  wrappers, resource concepts, the public `DecodeTextError` and `PathError`
  enums, the public `std.time.milliseconds` wrapper, and the complete public
  `std.log` graph. Time construction now resolves through an ordinary source
  `DefId`; only its exact-owner private primitive remains below source. Logging
  resolves through ordinary source `DefId` values and has no universal-value
  native fallback;
  only its exact-owner private typed write primitive remains compiler-owned.
  `std.json.format_json` traverses Json and builds canonical UTF-8 through
  ordinary Loom source. Its generic packed builder is `Bytes.add`: a mutable
  Bytes binding has copy-on-write value semantics, checked byte units, hidden
  geometric capacity, exact lower/upper LCIR guard proofs, and an independently
  validated unique-push form. Source List/Bytes copies become zero-code
  `CollectionShare` SSA boundaries so COW aliases cannot inherit uniqueness.
  The compiler, LLVM backend, runtime ABI, and runtime contain no
  JSON-formatting opcode, layout descriptor, or entry point.
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
  remain below source. Interpreted MIR artifact 45 and persistent cache schema
  18 reject infinite value layouts, source-impossible mutable parameter slots
  and coroutine receivers, invariant-boundary bypasses, removed semantic types,
  fixed slots, and special cleanup tags instead of decoding them through
  compatibility paths.
  Native runtime ABI 39 (`runtime-v33`, `stdlib-v9`, `text-v4`, and
  `typed-bytes-v2`) adds checked packed Bytes growth and non-collecting
  ByteObject-to-Text decode. ABI 38 removed the former typed JSON-formatting
  boundary. ABI 37 deleted the unreachable universal `ValueSlot` heap and root
  chain, runtime witness arena, legacy Task/value operations, and Int-list
  implementation, following the earlier removal of universal File, Socket,
  close, logging, and process boundaries.
  The live shared join/fault scheduler operations retain `task-v2`.
  The 16-byte `typed-io-v1` outcome uses its primitive
  payload for either a resource token or a closed fault class, preserving
  `InvalidPort`, `SocketResolveFault`, and operation-specific host faults; the
  `typed-resource-v1` close boundary remains unchanged.
  `std.process.arguments` builds its List in source over typed snapshot
  primitives; process input has no universal-value or checked-MIR runtime path,
  and Windows snapshots the operating system's wide arguments instead of the
  lossy narrow C entry vector.
  JSON parsing and canonical formatting have no compiler opcode or runtime
  entry point; both execute through ordinary `std.json` Loom source.
- A precise moving collector, lazy single-threaded executor, OS reactor, bounded
  blocking pool, structured cancellation, deterministic cleanup, and strict
  runtime-bundle identity checks.
- A dedicated typed-I/O fixture closes real `check`, `build`, `test`, and `run`
  commands and rejects universal I/O symbols in its object. The integrated
  standard-library native test now uses the production backend and exercises real
  filesystem and loopback-socket I/O.
- Cross-platform CI, release smoke gates, fuzz and conformance checks, and
  reproducible native benchmarks against C, C++, Go, and Rust.

### Current boundary

- The project accepts only its current source language, lockfile, library,
  artifact, cache, and runtime-bundle formats. There are no aliases, upgrade
  readers, dual-format writers, deprecated runtime shims, or compatibility
  promises for unpublished formats.
- Typed LCIR is the destination compiler path. A complete checked LCIR
  artifact is now the sole input to LLVM; the universal-value checked-MIR
  emitter and route policy have been deleted. Unsupported reachable semantics
  are explicit native compilation errors. Task-free fields inside Task-bearing
  products use atomic typed projection/update operations, and list literals up
  to 65,536 elements use one typed backing allocation with iterative stores.
- Reachable File, Socket, logging, process, and Task operations follow the same
  whole-artifact typed lowering rule. Unreachable helpers do not affect native
  preparation or object identity. The runtime archive exports only the typed
  native boundary; it retains no dormant universal-value compatibility ABI.
- `std.json.parse_json` is a source-backed, iterative, depth-bounded parser that
  lowers completely through typed LCIR. The controlled standard-library
  fixture has no native-backend exception.
- Live programming, AST editing, AOP/advice, operator runtimes, runtime
  conformance discovery, ownership/borrow syntax, and a multithreaded executor
  are outside the current language.
