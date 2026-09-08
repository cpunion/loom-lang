# Loom for VS Code

A small development extension: highlighting, brackets/comments, unsaved-buffer
diagnostics, checked type hover, go to definition, and document formatting. The
language server runs the Loom compiler; JavaScript does not parse or type-check
Loom. Completion, references, rename, and incremental semantic caching are not
implemented.

## Try it

Build the [compiler](../../compiler/README.md#build-and-try-it), then:

```sh
cd editors/vscode
npm ci
code .
```

Press F5 to launch the **Loom extension** development host with the receipt
example folder. In that new window,
set `loom.executable` to your absolute `target/loom` path (`loom.exe` on Windows)
and `loom.stdRoot` to the repository's `compiler/std`. Open `main.loom`, introduce
a type error without saving, then fix it. Use **Format Document** or enable
format-on-save with `cpunion.loom-language` as the Loom default formatter.
No extension is installed or published by these commands.

`loom.executable` defaults to `loom` on PATH; relative paths containing a separator
and relative `loom.stdRoot` paths resolve from the containing workspace folder.
The configured compiler must support `editor-check`, `editor-query`, and `fmt --stdin`. Compiler
execution requires a trusted, local-filesystem workspace; highlighting also works
in restricted mode. Remote VS Code workspaces run the extension on the remote host.

### A small receipt project

Open [receipt/main.loom](../../compiler/examples/receipt/main.loom), with its
[same-directory unit tests](../../compiler/examples/receipt/main_test.loom).
From the repository root:

```sh
target/loom test compiler/examples/receipt
target/loom run compiler/examples/receipt
```

The program prints `36`. Change `quantity = 3` to `quantity = 4` and run again
to get `48`. Try `quantity = "three"` without saving to see the compiler's type
diagnostic, then restore the integer. Hover over an expression for its checked
type or use **Go to Definition** on a resolved name. Use **Format Document** or format-on-save
to keep fields, constructors, and statements readable as you edit.

For a second exercise, open [loops/main.loom](../../compiler/examples/loops/main.loom).
Run `target/loom test compiler/examples/loops` and `target/loom run compiler/examples/loops`
from the repository root; the program prints `8`. Change the limit from `10` to
`4` and run it again to get `4`. Move `break` outside the loop to see its scope
diagnostic, then restore it. The example uses `[1, -9, 3, 4, 8]`; the tests also
pass `[]` to a `List[Int]` parameter. Try replacing one integer with `true` to
see an element-type diagnostic.

## Behavior and tests

After a 350 ms debounce, the server checks each open directory package with tests
enabled. Every request includes snapshots of all open Loom file buffers, including
new files whose parent directories exist. Imported dependencies and package rules
remain the compiler's responsibility. Snapshots use a private temporary directory
and are removed after completion/cancellation; user source files are never changed.
An edit cancels outstanding checking and semantic queries, including queries in
other open files. Configuration and watched-file changes also invalidate queries.
Formatting edits are discarded if the document changes or the request is canceled.
Hover and definition perform fresh compiler checks with the same snapshots. If
checking fails or no checked information exists, they return no result rather than
guessing from names. When checked generic instances have different types or
targets, all results are retained.

This is checked-body navigation, not a complete symbol index. Signatures, type
annotations, uninstantiated bodies, and folded code without source identity have
no result. Some names, such as match-bound locals, have hover but no definition.
Dynamic calls navigate to concept declarations, not a guessed runtime
implementation; ambiguous concept overloads omit the definition.

```sh
npm test           # Real LSP transport with a small process fixture
npm run smoke      # Real target/loom: overlays, hover/definition, formatting
npm run smoke:host # Installed VS Code: actual extension activation and commands
```

The extensionless `test/fixtures/project/editor-check`, `editor-query`, and `fmt` files are small
Node process fixtures for `npm test`, not generated compiler artifacts. They are
excluded from extension packaging; both smoke commands use the real compiler.

The smoke command accepts `LOOM_EDITOR_COMPILER` and `LOOM_EDITOR_STD` overrides.
The optional host test opens and closes its own Development Host with temporary
user-data/extensions directories; it neither installs the extension nor changes
your settings. `VSCODE_EXECUTABLE` selects an installed VS Code launcher (use the
absolute `Code.exe` path on Windows). No VS Code download is performed.
`Loom` in the Output panel contains server messages. Protocol tracing is opt-in
and can contain source text. Checks currently stop at the compiler's first error;
they are fresh checks, not a persistent compiler session.

The adapter calls:

```text
loom editor-check PACKAGE --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
loom editor-query PACKAGE --at ORIGINAL UTF8_BYTE_OFFSET --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
loom fmt --stdin
```

Check output is `{ "diagnostics": [{ "path", "message", "start", "end" }],
"error"?: "project error" }`. Query output also includes `"hover": null | {
"start", "end", "types": ["Int", ...] }` and `"definitions": [{ "path", "start",
"end" }]`. Spans and query offsets use UTF-8 bytes; the server maps them to/from
LSP UTF-16 positions using the captured text. Formatting reads and writes source
on stdin/stdout.

The client/server use Microsoft's [Language Server SDK](https://github.com/microsoft/vscode-languageserver-node)
and follow the [VS Code extension guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide).
The TextMate grammar is only lexical highlighting, following the
[syntax highlighting guide](https://code.visualstudio.com/api/language-extensions/syntax-highlight-guide).
