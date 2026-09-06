# Implementation status

Only the native compiler under `compiler/` is maintained. Its
[guide](../../compiler/README.md) lists the tested subset and commands.
The former workspace compiler and its feature matrices have been removed.

The active compiler has a real source-to-native check/build/test/run path,
package/test isolation, concrete generic records/enums, shared lists, UTF-8
text, real file reads/writes and stdout, scalar constrained construction, and
mandatory postconditions within a bounded proof fragment. A Loom-written
frontend reads real source and returns recursive syntax trees; managed tests force
collection before every allocation. Constants and safe scalar weakening avoid
redundant checks; unknown construction returns an ordinary source `Result`.
Postfix `?` propagates errors with ordinary enum control flow. Recursive data
through `List` is supported; infinite inline layouts and growing generic
specializations reject.

The native gate uses macOS, Rust 1.88, and LLVM 19. Compiler, native integration,
and runtime tests cover real check/build/test/run, required-proof rejection,
input-boundary failures, and allocation-free scalar/record paths.
The [roadmap](../../ROADMAP.md) puts binding and typing next in N1.
[Source handling, lexing, syntax parsing, and positioned
diagnostics](../../compiler/loom/README.md) now run as native Loom code over the
frontend's own sources, `std`, and examples. The Rust seed still builds the
tool; it cannot yet produce another compiler stage.

Accepted language and deployment decisions remain targets, not claims that the
whole design is implemented. No release, full platform matrix, complete
standard library, or self-hosting claim is made.
