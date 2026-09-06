# Changelog

Loom has not published a release. This records changes to the active
implementation, not a compatibility ledger for previous prototypes.

## Unreleased

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
  The root workspace and macOS gate now serve the native compiler only.
  The removed work remains recoverable in Git history.

The [roadmap](ROADMAP.md) tracks incomplete goals separately.
