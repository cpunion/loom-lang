# Compiler internals

These documents describe the current Loom implementation for compiler and
runtime contributors. They are not a second language specification. Types,
layouts, symbols, and optimization heuristics described here may change when
the corresponding versioned boundary changes.

## Architecture

The workspace is split into narrow crates:

| Crate | Responsibility |
| --- | --- |
| `loom-core` | Shared identities, spans, diagnostics, and language version. |
| `loom-syntax` | Lossless lexing, parsing, recovery, AST, and formatting support. |
| `loom-hir` | Source-independent declaration/body identities. |
| `loom-sema` | Names, types, places, contracts, concepts, and witness proofs. |
| `loom-mir` | Typed executable IR, artifact encoding, liveness, and validation. |
| `loom-lowering` | Total lowering from typed HIR to MIR. |
| `loom-interpreter` | Deterministic execution of validated MIR. |
| `loom-runtime-abi` | Shared compiler-private native ABI constants and C-shaped records. |
| `loom-runtime` | Moving GC, values, cleanup support, scheduler, reactor, and I/O workers. |
| `loom-codegen-llvm` | Reachability, native layouts, LLVM emission, objects, linking, and runtime bundles. |
| `loom-driver` | Projects, resolution, source snapshots, diagnostics, and persistent cache. |
| `loom-cli` | `loomc` host boundary and process execution. |
| `loom-lsp` | Language-server integration over driver snapshots. |
| `loom-quality` | Controlled end-to-end evidence gates. |
| `loom-benchmark` | Cross-language microbenchmark runner. |

## Contents

- [Compiler pipeline](compiler-pipeline.md)
- [MIR and validation](mir-and-validation.md)
- [Reachability and dead-code elimination](reachability-and-dce.md)
- [LLVM backend](llvm-backend.md)
- [Value layout and native ABI](value-layout-and-native-abi.md)
- [GC runtime](gc-runtime.md)
- [Async runtime](async-runtime.md)
- [Incremental cache](incremental-cache.md)

## Invariant

Backends receive only checked MIR. They do not resolve names, infer
conformances, reinterpret contracts, or repair malformed IR. A MIR validation
failure after successful source analysis is a compiler defect; malformed
artifact or cache input is rejected at the boundary.

Implementation status and platform support are maintained separately in
[Implementation status](../project/implementation-status.md). Future design
ideas belong in an RFC or roadmap, not in these implementation documents.
