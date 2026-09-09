# Loom for VS Code

A small development extension: highlighting, brackets/comments, unsaved-buffer
diagnostics, name/member completion, checked type hover, go to definition, and document formatting. The
language server runs the Loom compiler; JavaScript does not parse or type-check
Loom. References, rename, and incremental semantic caching are not
implemented.

## Try it

Build the [compiler](../../compiler/README.md#build-and-try-it). With Node.js,
npm, and VS Code installed, run from the repository root:

```sh
npm --prefix editors/vscode ci
npm --prefix editors/vscode run try
```

On an already built checkout with dependencies installed, only the second command
is needed. It opens a Development Host directly with the
[file-tool workspace](wordcount.code-workspace), using this checkout's compiler
and Loom-only format-on-save. No F5 setup, extension installation, or global
configuration changes are required. The trial edits real example files in this
checkout; review or keep your changes as usual.

Open `main.loom` and follow the [short programming exercise](../../compiler/examples/wordcount/README.md).
**Tasks: Run Task** offers format, check, build, test, and run; test includes the
separate `stats` package, and run prints `2 4 23`. Introduce an unsaved type error,
fix it, hover over a value, navigate to its definition, and save to format.

If `code` is unavailable, set `VSCODE_EXECUTABLE` to the installed VS Code
launcher. Alternatively, open `editors/vscode` using **File > Open Folder**,
select **Loom file-tool trial**, and press F5. Select **Loom extension** instead
for the smaller [receipt workspace](try-loom.code-workspace), whose run task
prints `36`.

For your own project, set workspace `loom.executable` to the absolute `target/loom`
path (`loom.exe` on Windows), or a [staged toolchain's](../../compiler/README.md#relocatable-local-toolchain)
`bin/loom`. Leave `loom.stdRoot` empty to use that compiler's std; an explicit
path overrides discovery.
The checkout's compiler can also run CLI commands directly in your project
directory; use its absolute path or a relative path containing a separator.
Do not assume a bare `loom` on PATH is this compiler: `target/loom --help` should
start with `Loom source compiler`. The trial does not replace other installed
tools. Bare-command discovery needs an explicit `loom.stdRoot`; prefer the
absolute compiler path. This is still a development extension, not a Marketplace release.

`loom.executable` defaults to `loom` on PATH; relative paths containing a separator
and relative `loom.stdRoot` paths resolve from the containing workspace folder.
Files outside a single-folder workspace use that folder's configuration and path
base, including standard-library files opened through navigation. For external
files in a multi-root workspace, use absolute paths or a compiler on PATH; the
server does not guess a folder for relative settings. With no workspace folder,
relative paths resolve from the document's directory.
The configured compiler must support `editor-check`, `editor-query`, `editor-complete`, and `fmt --stdin`. Compiler
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
Closure parameters and captured bindings navigate through checked source
identities, without exposing private capture cells as user-facing types.

Name completion uses the compiler's package bindings and source scopes without
running type checking. It includes earlier locals, parameters, generic parameters,
match bindings, package-private declarations, imported public names and builtin
types. Inner bindings hide outer names; overloads retain separate declaration
signatures. Test-only names never enter production scope. Partial identifiers and
body type errors work; choosing a candidate replaces the whole identifier, even
when the cursor is in its middle. These are visible candidates, not a claim that
each overload or value is valid at this expression. A cursor-local recovery can fill one missing
name/value and close unmatched delimiters at EOF in a virtual completion snapshot.
It does not change the user's buffer, relax ordinary parsing/checking, or clear
the original syntax diagnostic. Other syntax/lexical errors still block results;
recovery is not a general error-tolerant parser. Strings and comments offer no
names. Requests reuse all unsaved buffers and cancellation
rules; completion results are not cached across edits.

After `.`, member completion infers the receiver from the enclosing signature and
preceding statements. It offers visible record fields, tuple indices, methods
supported by explicit concept evidence (including generic bounds and `dyn`), and
`.await` for Tasks in async functions. Match bindings and determined `comptime if`
branches reuse the checker. Unknown receiver types, earlier typing errors,
undetermined compile-time branches and `comptime` execution blocks have no result.
These are type-based hints, not verification of the unfinished body, callees,
contracts or resource flow; method overloads retain declaration signatures and
still require arguments. Names and members replace the entire partial token.

Qualified completion walks the same visible spellings as ordinary name lookup:
`std.` can offer `text`, and `std.text.` offers the symbols explicitly imported
from that package, not every public declaration in downloaded dependencies.
Current-package private symbols and test-only imports retain their normal scope;
same-named dependency instances do not merge. Local value receivers take priority
over namespaces, including whole-value match bindings. Type annotations and
constructors offer types; `dyn` offers concepts; type parameters hide matching
namespace roots. These are spelling candidates, not validated instantiations.
Inside an import statement, completion also discovers the current module, direct
dependencies, `std`, directory segments and public production declarations. It
uses unsaved overlays and the ordinary offline resolver; missing or changed Git
snapshots offer no declarations until explicitly resolved. It does not fetch,
write locks, follow nested-module/directory aliases, or include private/test
declarations. Malformed target files are skipped; candidates are not checked
package validity. Automatic imports remain unimplemented.

```sh
npm test           # Real LSP transport with a small process fixture
npm run smoke      # Real target/loom: overlays, hover/definition, completion, formatting
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
loom editor-complete PACKAGE --at ORIGINAL UTF8_BYTE_OFFSET --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
loom fmt --stdin
```

Check output is `{ "diagnostics": [{ "path", "message", "start", "end" }],
"error"?: "project error" }`. Query output also includes `"hover": null | {
"start", "end", "types": ["Int", ...] }` and `"definitions": [{ "path", "start",
"end" }]`. A response with diagnostics (exit code 1) may still contain checked
query results; a project `"error"` has none. Spans and query offsets use UTF-8 bytes;
the server maps them to/from LSP UTF-16 positions using the captured text.
Formatting reads and writes source on stdin/stdout.
Completion output adds `"completion": null | { "start", "end", "items": [
{ "label", "kind": "variable" | "function" | "type" | "field" | "method" | "keyword" | "namespace", "detail" }, ...] }`.
Its spans are also UTF-8 bytes; it does not run proofs or produce an executable.

The client/server use Microsoft's [Language Server SDK](https://github.com/microsoft/vscode-languageserver-node)
and follow the [VS Code extension guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide).
The TextMate grammar is only lexical highlighting, following the
[syntax highlighting guide](https://code.visualstudio.com/api/language-extensions/syntax-highlight-guide).
