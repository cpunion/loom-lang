# Loom-written compiler

The native Loom frontend loads packages, parses source, binds names, checks
types and required contracts, specializes reachable functions, and sends a
checked program to the single LLVM backend. It does not invoke the Rust source
parser, checker, or prover. The retained `loom-native` tool uses Rust/Inkwell for
LLVM lowering and host linking; it accepts checked IR, not Loom source.

## Bootstrap

From the repository root, after [bootstrapping the compiler](../README.md):

```sh
bash scripts/bootstrap.sh --dev
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

## Public syntax libraries

The compiler and ordinary Loom programs use the same source implementation:

| Package | Public entry points and data |
| --- | --- |
| `std.loom.source` | `Span`, `Diagnostic`, `Position`, `position`, `render` |
| `std.loom.lexer` | `lex`, `Token`, `Kind` |
| `std.loom.ast` | `Node`, `NodeKind`, `has` (direct-child lookup) |
| `std.loom.parser` | `parse(Text) Result[Node, Diagnostic]` |

Import, for example, `std.loom.parser.parse` and `std.loom.ast.NodeKind` in
any package. Parsing supplied text returns a file node or the first diagnostic;
it does not read files, load a project, bind names, check types, or invoke the
compiler CLI/backend. The standalone
[syntax example](../examples/syntax/main.loom) inspects declarations, spans,
and errors using only ordinary `std` imports:

```sh
target/loom test compiler/examples/syntax
target/loom run compiler/examples/syntax
```

Spans are half-open UTF-8 byte ranges in the supplied text. `position` requires
a valid UTF-8 boundary and returns 1-based lines and Unicode scalar columns,
not terminal-cell columns. Positions are revision-relative, not persistent
definition identities. The parser implements the current syntax subset;
successful parsing does not establish type or contract validity.

This is an evolving public API, not a stable node schema or lossless editor
tree: comments and formatting trivia are discarded, and string token values are
decoded. Preserve original source when tooling needs its spelling and layout.
Typed semantic queries, metaprogramming, and identity-aware editing remain
later library boundaries in the [roadmap](../../ROADMAP.md).

## Public project and binding libraries

Project analysis is opt-in; in-memory syntax users do not import these layers:

- `std.loom.manifest.parse(text)` returns `Result[Module, Text]`, with module
  name/version metadata. It parses the supported manifest subset, not general
  TOML or registry/dependency resolution.
- `std.loom.project.load(path, std_root, tests)` returns `Result[Project, Text]`.
  `Project` contains `files List[SourceFile]` and a root package name. It reads
  the selected directory package and its import closure, not the whole
  repository. Only the selected root contributes tests when requested.
- `std.loom.binding.bind(files, root, tests)` returns `Result[Program, Failure]`
  after declaration/import validation. Inspect `Program.symbols` for
  declarations; `Failure.source` identifies the input file for its diagnostic.

Package identities follow directories. Root and `std` paths are canonicalized;
imported directory symlink aliases that change package identity are rejected.
Source-file symlinks remain allowed, with trust based on canonical file paths.

`candidates(program, file, test_only, path)` returns indices into that program's
symbol table, respecting the supplied file's visibility and test context.
These are name/overload candidates, not the selected function at a call site;
binding does not type-check expressions or prove contracts. `Symbol.file`
indexes `Program.files`, and `Symbol.node.span` locates the declaration there.
The project's `name`, `package`, and `qualify` helpers manipulate qualified
names, not filesystem paths.

The standalone [project example](../examples/project/main.loom) loads a user
package, reports root declarations and source positions, and renders failures
without invoking the compiler or a backend child process:

```sh
target/loom build compiler/examples/project --output target/project
target/project compiler/examples/data compiler/std --tests
target/loom test compiler/examples/project
```

File and symbol indices belong to one analysis result; they are not stable
definition identities. Reload/rebind after editing input files or trees: the
lookup tables are not an editing model or incremental compilation cache. The
compiler uses these same libraries, with no private copies or wrapper APIs.

## Compiler packages

The remaining directories are internal packages in the `frontend` module:

- `typed`, `checking`: checked types, expressions, concrete function instances,
  private runtime signatures and required-proof obligations.
- `proof`: bounded scalar reasoning with mathematical integers. Unsupported
  required proofs reject; there is no runtime postcondition fallback.
- `artifact`: a private counted UTF-8 stream to the LLVM tool, not a stable
  package, cache or public AST format. Proof-only locals are not emitted.
- Root: CLI orchestration using source `std.fs`, `std.file`, `std.io` and
  `std.process`. Process arguments are literal; no shell is implicitly invoked.

This is the self-hosting subset, not the complete accepted language. Broader
contracts, concepts, compile-time programming, resources, Tasks and module
resolution remain in [implementation status](../../docs/project/implementation-status.md).
