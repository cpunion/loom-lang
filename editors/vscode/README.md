# Loom for VS Code

A small development extension: highlighting, brackets/comments, unsaved-buffer
diagnostics, and document formatting. The language server runs the Loom compiler;
JavaScript does not parse or type-check Loom. Completion, navigation, rename, and
incremental semantic caching are not implemented.

## Try it

Build the [compiler](../../compiler/README.md#build-and-try-it), then:

```sh
cd editors/vscode
npm ci
code .
```

Press F5 to launch the **Loom extension** development host. In that new window,
set `loom.executable` to your absolute `target/loom` path (`loom.exe` on Windows)
and `loom.stdRoot` to the repository's `compiler/std`. Open `main.loom`, introduce
a type error without saving, then fix it. Use **Format Document** or enable
format-on-save with `cpunion.loom-language` as the Loom default formatter.
No extension is installed or published by these commands.

`loom.executable` defaults to `loom` on PATH; relative paths containing a separator
and relative `loom.stdRoot` paths resolve from the containing workspace folder.
The configured compiler must support `editor-check` and `fmt --stdin`. Compiler
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
diagnostic, then restore the integer. Use **Format Document** or format-on-save
to keep fields, constructors, and statements readable as you edit.

## Behavior and tests

After a 350 ms debounce, the server checks each open directory package with tests
enabled. Every request includes snapshots of all open Loom file buffers, including
new files whose parent directories exist. Imported dependencies and package rules
remain the compiler's responsibility. Snapshots use a private temporary directory
and are removed after completion/cancellation; user source files are never changed.
An edit cancels outstanding checking and suppresses stale diagnostics. Formatting
edits are discarded if the document changes or the request is canceled.

```sh
npm test           # Real LSP transport with a small process fixture
npm run smoke      # Real target/loom: unsaved errors, sibling overlay, formatting
npm run smoke:host # Installed VS Code: actual extension activation and commands
```

The extensionless `test/fixtures/project/editor-check` and `fmt` files are small
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
loom fmt --stdin
```

Check output is `{ "diagnostics": [{ "path", "message", "start", "end" }],
"error"?: "project error" }`, with UTF-8 byte spans. The server maps these spans
to LSP UTF-16 positions. Formatting reads and writes source on stdin/stdout.

The client/server use Microsoft's [Language Server SDK](https://github.com/microsoft/vscode-languageserver-node)
and follow the [VS Code extension guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide).
The TextMate grammar is only lexical highlighting, following the
[syntax highlighting guide](https://code.visualstudio.com/api/language-extensions/syntax-highlight-guide).
