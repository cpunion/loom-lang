# Loom for VS Code

A small development extension: highlighting, brackets/comments, unsaved-buffer
diagnostics, checked type hover, go to definition, and document formatting. The
language server runs the Loom compiler; JavaScript does not parse or type-check
Loom. Completion, references, rename, and incremental semantic caching are not
implemented.

## Try it

Build the [compiler](../../compiler/README.md#build-and-try-it). With Node.js,
npm, and VS Code installed, run from the repository root:

```sh
cd editors/vscode
npm ci
code .
```

If the `code` shell command is unavailable, use VS Code's **File > Open Folder**
to open `editors/vscode` after `npm ci`.

Press F5 to launch the **Loom extension** development host with the
[trial workspace](try-loom.code-workspace). Compiler paths and Loom-only
format-on-save are already configured there; your user settings are unchanged.
Open `main.loom`, introduce a type error without saving, then fix it. Hover over
a value, use **Go to Definition**, and save to format. **Tasks: Run Task** offers
**Loom: check receipt**, **Loom: test receipt**, and **Loom: run receipt**; the run
task prints `36`. No extension is installed or published by these commands.

For your own project, set workspace `loom.executable` to the absolute `target/loom`
path (`loom.exe` on Windows) and `loom.stdRoot` to the repository's `compiler/std`.
The checkout's compiler can also run CLI commands directly in your project
directory; use its absolute path or a relative path containing a separator.

For a more realistic exercise, select **Loom file-tool trial** before pressing
F5. Its [workspace](wordcount.code-workspace) opens a small
[file-counting application](../../compiler/examples/wordcount/README.md) with a
separate `stats` package. Its tasks check, test both packages, and run against a
UTF-8 file. Try the edits in that walkthrough, including a failed test and missing
file; `run` prints `2 4 23` before changes. This is still a development extension,
not an installed Marketplace release.

`loom.executable` defaults to `loom` on PATH; relative paths containing a separator
and relative `loom.stdRoot` paths resolve from the containing workspace folder.
Files outside a single-folder workspace use that folder's configuration and path
base, including standard-library files opened through navigation. For external
files in a multi-root workspace, use absolute paths or a compiler on PATH; the
server does not guess a folder for relative settings. With no workspace folder,
relative paths resolve from the document's directory.
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
Canonical formatting puts each record field on its own line. Loom currently
uses newlines, not semicolons; the formatter expands compact field declarations.

For a second exercise, open [loops/main.loom](../../compiler/examples/loops/main.loom).
Run `target/loom test compiler/examples/loops` and `target/loom run compiler/examples/loops`
from the repository root; the program prints `8`. Change the limit from `10` to
`4` and run it again to get `4`. Move `break` outside the loop to see its scope
diagnostic, then restore it. The example uses `[1, -9, 3, 4, 8]`; the tests also
pass `[]` to a `List[Int]` parameter. Try replacing one integer with `true` to
see an element-type diagnostic.
The loop reads `values[index]`; its tests also update a shared alias with
`alias[1] = 7`. Change that replacement to `true` to see a type error, then
restore it and run the tests again.

## Behavior and tests

After a 350 ms debounce, the server checks each open directory package with tests
enabled. Every request includes snapshots of all open Loom file buffers, including
new files whose parent directories exist. Imported dependencies and package rules
remain the compiler's responsibility. Snapshots use a private temporary directory
and are removed after completion/cancellation; user source files are never changed.
An edit cancels outstanding checking and semantic queries, including queries in
other open files. Configuration and watched-file changes also invalidate queries.
Formatting edits are discarded if the document changes or the request is canceled.
Hover and definition perform fresh compiler checks with the same snapshots.
Diagnostics can coexist with checked query results: an unrelated non-generic
function-body error, even in the same file, need not hide an independently checked
concrete function and its dependencies. Errors in that function or its dependencies
return no result; there is no recovery within an erroneous function. Syntax,
global declaration, and template errors can still prevent queries. The compiler
keeps the original bindings and never guesses from names. After a successful
whole-package check, all differing types and targets from checked generic instances
are retained.

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
The optional host test opens and closes its own Development Host with a temporary
project and user-data/extensions directories. It also verifies format-on-save and
same-directory tests plus execution; it neither installs the extension nor changes
your settings or sources. Success requires both app exit and an explicit completion
report written after every host assertion and cleanup; extension activation alone
is not a pass. The launcher captures VS Code logs and prints their tail on failure.
`VSCODE_EXECUTABLE` selects an installed VS Code launcher (use the
absolute `Code.exe` path on Windows). No VS Code download is performed.
`Loom` in the Output panel contains server messages. Protocol tracing is opt-in
and can contain source text. Checks currently stop at the compiler's first error;
they are fresh checks, not a persistent compiler session.
Failure to start a package check is reported for that package and does not clear
diagnostics from other successfully checked packages.

The adapter calls:

```text
loom editor-check PACKAGE --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
loom editor-query PACKAGE --at ORIGINAL UTF8_BYTE_OFFSET --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
loom fmt --stdin
```

Check output is `{ "diagnostics": [{ "path", "message", "start", "end" }],
"error"?: "project error" }`. Query output also includes `"hover": null | {
"start", "end", "types": ["Int", ...] }` and `"definitions": [{ "path", "start",
"end" }]`. A response with diagnostics (exit code 1) may still contain checked
query results; a project `"error"` has none. Spans and query offsets use UTF-8 bytes;
the server maps them to/from LSP UTF-16 positions using the captured text.
Formatting reads and writes source on stdin/stdout.

The client/server use Microsoft's [Language Server SDK](https://github.com/microsoft/vscode-languageserver-node)
and follow the [VS Code extension guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide).
The TextMate grammar is only lexical highlighting, following the
[syntax highlighting guide](https://code.visualstudio.com/api/language-extensions/syntax-highlight-guide).
