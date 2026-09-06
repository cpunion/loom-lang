# Loom-written frontend

The N1 frontend is native Loom code: source positions, diagnostics, lexing, and
syntax parsing for the seed's current language. The Rust seed builds it; it
does not call the Rust lexer or parser while running. This is not yet a binder,
type checker, or self-hosted compiler.

From the repository root, after building the [seed](../README.md):

```sh
target/debug/loom check compiler/loom
target/debug/loom build compiler/loom --output target/loom-front
target/loom-front lex compiler/loom/main.loom compiler/loom/lexer/lexer.loom
target/loom-front parse compiler/loom/main.loom compiler/loom/parser/core.loom
LOOM_GC_STRESS=1 target/debug/loom test compiler/loom/lexer
LOOM_GC_STRESS=1 target/debug/loom test compiler/loom/source
LOOM_GC_STRESS=1 target/debug/loom test compiler/loom/parser
```

`loom-front` is a development artifact name, not another supported compiler
command. `lex` reads one or more files, reports token counts excluding EOF on
stdout. `parse` reports top-level declaration counts, including imports.
Both send diagnostics to stderr. Exit codes are 0 for success, 1 for input/output
or syntax failure, and 2 for invalid command usage.

Each directory is a package under the `frontend` module:

- `source`: byte spans and 1-based line/Unicode-scalar columns, including CRLF,
  CR, and LF. Display columns are not terminal cell widths.
- `lexer`: Unicode identifiers, whitespace/comments, operators, numbers as
  lexemes, UTF-8 strings and escape decoding. Tokens retain byte spans and
  decoded string values; only the final token is EOF.
- `syntax`: recursive nodes with explicit kinds, values, spans, and ordered
  children. These are compiler data, not the representation of user values.
- `parser`: declarations, types, contracts, statements, expressions, and flat
  patterns. Parsing functions return the next cursor and syntax node; errors
  propagate with `?`. Binding, type checking, and proof remain separate stages.
- Root package: file/argument handling and diagnostics through source `std`.

UTF-8 decoding/encoding, integer rendering, tokenization, and I/O loops are Loom
code. Unicode property tables currently use a private Rust standard-library
bridge; they contain no Loom lexical policy. Character classifications follow
the Rust toolchain used to build the runtime.

Native integration tests run this tool over its own, `std`, and example source
files. Focused package and error-path tests run with forced GC. The former
ASCII-only scanner example has been removed rather than maintained in parallel.
Next are binding, typing, and the staged bootstrap gates in the
[roadmap](../../ROADMAP.md).
