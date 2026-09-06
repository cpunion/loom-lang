# Changelog

Loom has not published a release. This records changes to the active
implementation, not a compatibility ledger for previous prototypes.

## Unreleased

- Implement the Windows MSVC native/runtime path, Unicode/binary I/O, canonical
  package paths, and executable suffixes. Add checked compiler export and
  same-workflow bootstrap transfer into a Windows CI gate; native Windows
  verification is still pending.
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
