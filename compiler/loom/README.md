# Loom-written compiler

The native Loom frontend loads packages, parses source, binds names, checks
types and required contracts, specializes reachable functions, and sends a
checked program to the single LLVM backend. It does not invoke the Rust source
parser, checker, or prover. The retained `loom-native` tool uses Rust/Inkwell for
LLVM lowering and host linking; it accepts checked IR, not Loom source.

## Bootstrap

From the repository root, after [bootstrapping the compiler](../README.md):

```sh
target/loom check compiler/examples/scalar
target/loom run compiler/examples/data
target/loom test compiler/loom/checking
target/loom test compiler/std/text
```

Stages 0 through 3 are [bootstrap generations](../../ROADMAP.md#n1--move-the-compiler-into-loom),
not language versions or additional supported compilers. Stage 0 is an existing
Loom compiler, recovered from frozen history only when needed. New language
features do not require a parallel Rust implementation; only their use in the
compiler's own source must wait until the selected bootstrap compiler supports
them.

The source CLI provides `check`, `build`, `test`, and `run` for one directory
package. It defaults to `compiler/std` and `target/debug/loom-native` relative to
the working directory; use `--std` and `--native-tool` elsewhere. `build` accepts
`--output`; native commands also accept `--emit-ir`. Library builds produce an
object, and production excludes test files and test declarations.

`lex` and `parse` inspect one or more source files, reporting token/declaration
counts or positioned diagnostics. For example:

```sh
target/loom parse compiler/loom/main.loom
target/loom test compiler/loom/proof
LOOM_GC_STRESS=1 compiler/loom/proof/target/tests
```

The bootstrap integration gate compares stage 2 and 3 binaries, selected
diagnostics and executable results, then runs compiler and source `std` tests.
Agreement is evidence, not a proof of compiler correctness. macOS is the current
validation host; the source path and manifest helpers intentionally implement
only the documented subset, not all platforms or general TOML.

## Package boundaries

Each directory is an ordinary package in the `frontend` module:

- `source`, `lexer`, `syntax`, `parser`: UTF-8 source, byte spans, positioned
  diagnostics, tokens and recursive syntax. No filesystem or LLVM dependency.
- `manifest`, `loading`: module metadata and the selected directory/import
  closure; only the root contributes tests.
- `binding`: package visibility, imports and overload candidates.
- `typed`, `checking`: checked types, expressions, concrete function instances,
  private runtime signatures and required-proof obligations.
- `proof`: bounded scalar reasoning with mathematical integers. Unsupported
  required proofs reject; there is no runtime postcondition fallback.
- `artifact`: a private counted UTF-8 stream to the LLVM tool, not a stable
  package, cache or public AST format. Proof-only locals are not emitted.
- Root: CLI orchestration using source `std.fs`, `std.file`, `std.io` and
  `std.process`. Process arguments are literal; no shell is implicitly invoked.

These boundaries will also serve user-callable parser/AST and project-analysis
libraries, then typed metaprogramming. The current internal node schema is not
yet that public API: comment-preserving editing, semantic queries and durable
definition identities remain planned. See the [roadmap](../../ROADMAP.md).

This is the self-hosting subset, not the complete accepted language. Broader
contracts, concepts, compile-time programming, resources, Tasks and module
resolution remain in [implementation status](../../docs/project/implementation-status.md).
