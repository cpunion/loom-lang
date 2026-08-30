# Compiler internals

These documents describe the current Loom implementation for compiler and
runtime contributors. They are not a second language specification. Types,
layouts, symbols, and optimization heuristics described here may change when
the corresponding versioned boundary changes.

## Architecture

The workspace is split into narrow crates:

| Crate | Responsibility |
| --- | --- |
| `loom-core` | Shared identities, spans, diagnostic vocabulary, and language version. |
| `loom-syntax` | Lossless lexing, parsing, recovery, AST, and formatting support. |
| `loom-hir` | Source-independent declaration/body identities. |
| `loom-sema` | Names, types, places, contracts, concepts, and witness proofs. |
| `loom-mir` | Typed executable IR, artifact encoding, liveness, and validation. |
| `loom-lowering` | Total lowering from typed HIR to MIR. |
| `loom-interpreter` | Deterministic execution of validated MIR. |
| `loom-codegen-ir` | Checked-MIR source roots/reachability plus atomic direct MIR-to-LCIR selection for primitives, direct literal/managed text, closed products/sums, and established transparent values; typed-SSA builders, validation, artifact roots, exact managed-root planning, and insertion-order dumps. |
| `loom-runtime-abi` | Shared compiler-private native ABI constants, universal and typed root records, and precise typed object descriptors. |
| `loom-runtime` | Universal compatibility values, universal and typed moving-GC support, cleanup, scheduler, reactor, and typed-only I/O workers. |
| `loom-codegen-llvm` | Native layouts, checked-MIR and checked-LCIR object emission, linking, and runtime bundles. |
| `loom-driver` | Projects, resolution, source snapshots, diagnostics, and persistent cache. |
| `loom-cli` | `loom` host boundary and process execution. |
| `loom-lsp` | Language-server integration over driver snapshots. |
| `loom-quality` | Controlled end-to-end evidence gates. |
| `loom-benchmark` | Cross-language microbenchmark runner. |

## Contents

- [Compiler pipeline](compiler-pipeline.md)
- [Core, standard library, and runtime boundary](core-library-runtime-boundary.md)
- [MIR and validation](mir-and-validation.md)
- [Reachability and dead-code elimination](reachability-and-dce.md)
- [Code generation IR](codegen-ir.md)
- [LLVM backend](llvm-backend.md)
- [Value layout and native ABI](value-layout-and-native-abi.md)
- [GC runtime](gc-runtime.md)
- [Async runtime](async-runtime.md)
- [Incremental cache](incremental-cache.md)

## Invariant

The production interpreter and LLVM backend receive only checked MIR. They do
not resolve names, infer conformances, reinterpret contracts, or repair
malformed IR. A MIR validation failure after successful source analysis is a
compiler defect; malformed artifact or cache input is rejected at the
boundary. This is type-enforced: lowering, driver snapshots, checked-MIR cache
entries, terminal backend constructors, source reachability, native object
identity/emission, and portable-library encoding retain or require
`loom_mir::CheckedProgram`.

`loom-codegen-ir` can construct a complete direct `CheckedArtifact` through an
independently validated `CheckedProgram` and validated roots.
`loom-codegen-llvm` automatically selects that typed route for a completely
supported reachable artifact and otherwise prepares one whole checked-MIR
route when the graph contains no LCIR-only primitive. Reachable File or Socket
I/O fails closed if complete LCIR lowering is unavailable. The exact prepared
target and route-specific fingerprint are reused by the production cache and
emitter. Broader LCIR representation and semantic coverage remain tracked by the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md).

Implementation status and platform support are maintained separately in
[Implementation status](../project/implementation-status.md). Future design
ideas belong in an RFC or roadmap, not in these implementation documents.
