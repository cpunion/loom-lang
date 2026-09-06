# Implementation status

Only the native compiler under `compiler/` is maintained. Its
[guide](../../compiler/README.md) lists the tested subset and commands.
The former workspace compiler and its feature matrices have been removed.

The active compiler has a real source-to-native check/build/test/run path,
package/test isolation, concrete generic records/enums, shared lists, UTF-8
text, real file reads/writes and stdout, scalar constrained construction, and
mandatory postconditions within a bounded proof fragment. A Loom-written
scanner reads real source and returns typed tokens; managed tests force
collection before every allocation. Constants and safe scalar weakening avoid
redundant checks; unknown construction returns an ordinary source `Result`.

The N0 vertical-slice gate passes on macOS with Rust 1.88 and LLVM 19: 23 compiler
unit tests, 11 native integration tests, and 4 runtime tests. This includes real
check/build/test/run, required-proof rejection, input-boundary failures, and
allocation-free scalar/record paths. The [roadmap](../../ROADMAP.md) puts
Loom-written source handling, lexer/parser, and diagnostics next in N1.

Accepted language and deployment decisions remain targets, not claims that the
whole design is implemented. No release, full platform matrix, complete
standard library, or self-hosting claim is made.
