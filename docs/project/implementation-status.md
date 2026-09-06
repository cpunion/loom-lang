# Implementation status

Only the native compiler under `compiler/` is maintained. Its
[guide](../../compiler/README.md) lists the tested subset and commands.
The former workspace compiler and its feature matrices have been removed.

The active compiler has a real source-to-native check/build/test/run path,
package/test isolation, concrete generic records/enums, shared lists, UTF-8
text, real file reads, checked scalar arithmetic, and mandatory postconditions
within a bounded proof fragment. A Loom-written scanner reads real source and
returns typed tokens; managed tests force collection before every allocation.
The [roadmap](../../ROADMAP.md) defines the
remaining native and self-hosting gates.

Accepted language and deployment decisions remain targets, not claims that the
whole design is implemented. No release, full platform matrix, complete
standard library, or self-hosting claim is made.
