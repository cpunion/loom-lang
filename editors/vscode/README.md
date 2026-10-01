# Loom for VS Code

A small development extension: highlighting, brackets/comments, unsaved-buffer
diagnostics, name/member completion, checked type hover, go to definition, find references,
checked local and module-wide function/type rename, import quick fixes, and document
formatting. The
language server runs the Loom compiler; JavaScript does not parse or type-check
Loom. A resident compiler reuses unchanged checked snapshots for diagnostics,
hover and navigation, and unchanged ordinary checks and concrete bodies after edits.
Replacing or rebuilding the compiler restarts its worker on the next request;
queued requests remain serialized and cancellation still retires only that worker.
Edits across external module consumers are not implemented. Unknown or generated
references refuse checked rename instead of offering a partial edit.
CLI disk caching is separate and opt-in.

## Try it

With this extension installed and the checkout built, `code .` from the repository
root also works. The committed `.vscode/settings.json` selects `target/loom` and
`compiler/std` for compiler, library and example files. It enables Loom-only
format-on-save without changing global settings or relying on a bare `loom` on
PATH. A missing compiler requires a bootstrap build; intentional rejection
fixtures can still report real source errors.

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
Set `loom.buildOptions` to a string-valued object such as
`{ "app.channel": "preview" }`, matching CLI `--build-option app.channel=preview`.
Diagnostics and semantic queries share these explicit inputs; changing the
configuration cancels stale queries and rechecks open packages. Values may be
compiled into artifacts, so this setting must not contain secrets.
Compile-time `std.build.target` queries lazily use the backend discovered from
the compiler. Set `loom.nativeTool` to match an explicit CLI `--native-tool`;
relative paths follow the same workspace rules as `loom.stdRoot`.
The configured compiler must support `editor-check`, `editor-query`,
`editor-complete`, `editor-references`, `editor-rename`, `editor-auto-import`, and
`fmt --stdin`. Compiler
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
uses newlines for statements. Record fields require newlines or `;` separators;
the formatter expands compact declarations to one field per line.

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
Hover, definition, references, and rename perform fresh compiler checks
with the same snapshots.
Diagnostics can coexist with checked query results: an unrelated non-generic
function-body error, even in the same file, need not hide an independently checked
concrete function and its dependencies. Errors in that function or its dependencies
return no result; there is no recovery within an erroneous function. Syntax,
global declaration, and template errors can still prevent queries. The compiler
keeps the original bindings and never guesses from names. After a successful
whole-package check, all differing types and targets from checked generic instances
are retained.

This is checked-body navigation, not a complete symbol index. References use
exact definition spans from checked uses and can include the declaration. One
public top-level function in the current module also includes import declarations
and uses in other directory packages and their private tests. They omit
uninstantiated bodies, unchecked syntax and ambiguous targets. A package error suppresses references and rename until the
whole package checks; hover and definition retain their independent-function
fallback.

Rename edits checked runtime parameters and local `let`/`var`/`scoped`, tuple,
record and match bindings in ordinary named functions and methods. Contracts
and guarded arms use the same local identities as bodies; field labels and
enum variants are not local bindings. Every local edit is checked again in an
in-memory package before it is offered. It also renames
one production package-private top-level function or nominal type across files in the selected package when
every same-spelled token is an exact checked reference and the virtual edits pass
a full in-memory package check with test files included. One public top-level
function, record, enum or constrained type can be renamed throughout the current
module, updating imports and checked references. Function edits include bare and
qualified calls and callbacks; type edits include aligned function signatures,
record fields, enum payloads, local annotations, constructors and enum patterns.
Each directory is checked as its
own test root, including unopened tests and file-backed unsaved snapshots. The
virtual edit must pass every selected package's type and contract checks before
it is offered. Discovery uses the recursive test boundary: nested modules,
hidden/build directories and directory symlinks are excluded. This is a local
module edit, not an API migration for external consumers or dependency snapshots.
It refuses overloads, generated code, unaccounted occurrences, name collisions,
and invalid identifiers.
Runtime-local rename also works in compile-time-specialized functions and selected
branches when checked instances account for every occurrence. Unobserved uses,
nested closures and compile-time iteration still refuse the edit.
Compile-time parameter hover, navigation, references and rename use an on-demand fresh check that
records actual parameter bindings before constant/function specialization erases
them. Scalar values, callbacks and covered compile-time blocks/branches work;
unobserved branches, pack parameters or captures without complete binding evidence
refuse the edit. This does not add metadata to executable IR or bypass the final
virtual-package check. Navigation points to the parameter, not its specialized
callback target; hover reports concrete checked parameter types across observed
specializations. Uses erased in unobserved branches still provide no evidence.
Record fields can also be renamed through checked initializers, receivers,
record updates, explicit destructuring labels, contracts and type constraints.
Fields of public records use the same module-wide test-inclusive edit check;
private records stay within their package. Distinct checked locals and other
records' fields are not edited. Unchecked same-spelled accesses or constructor
labels refuse edits, even in a module package that has not imported the owner.
Concept rename is not yet available. Type aliases, unobserved type
applications and generated or unaligned annotations without complete checked
binding evidence refuse type edits. The server
returns a workspace edit for the
client to apply; it never writes source files directly.

Checked runtime parameter and match-binding declarations have hover and
definition evidence even when unused. Concrete type annotations use checked
signature and data types; generic/comptime parameters never navigate to a
same-spelled nominal declaration. Imports navigate through package bindings.
Uninstantiated bodies and folded code without source identity still have no result.
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
package validity.

For an unresolved explicit path such as `std.text.length(...)`, **Quick Fix** can
insert `import std.text.length`. A missing bare name can also offer an import
when exactly one public production declaration exports that spelling from the
selected module, `std`, or a declared direct dependency. The compiler checks the
revised in-memory package before offering the edit and places bare-name imports
after existing imports or leading comments. Private names, overloads, ambiguous
names, missing offline dependencies, and unrelated package errors produce no
action. It works with unsaved buffers and never writes files.

```sh
npm test           # Real LSP transport with a small process fixture
npm run smoke      # Real target/loom: overlays, navigation/references, local/private rename, completion, import Quick Fix, formatting
npm run smoke:host # Installed VS Code: actual extension activation and commands
npm run smoke:checkout # Repository-root settings and real source packages
```

The extensionless `test/fixtures/project/editor-*` and `fmt` files are small
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
and can contain source text. Checks stop at the compiler's first error.
Each executable/package pair uses a resident compiler with one successful checked
snapshot. Every request reloads the source closure and verifies observed build
input bytes, explicit options and target properties before reuse. Changed inputs
trigger a fresh check; errors and repaired completion snapshots are not cached.
Completion still performs its focused receiver check. Canceling an active request
restarts that worker; closing a package's last buffer or shutting down releases
its worker. Restart the language
server after replacing the compiler executable; configuration changes also restart
workers. After an edit, unchanged ordinary definitions can reuse abstract checks
while bindings, concrete instances and source locations are rebuilt. Reordering
or moving functions between same-package function-only files preserves reuse;
new/removed overloads invalidate affected callers. Nominal/import changes and
staged or resource-sensitive code conservatively recheck. This is in-memory
semantic-check reuse, not persistent per-definition native artifacts.
Failure to start a package check is reported for that package and does not clear
diagnostics from other successfully checked packages.

The adapter sends these commands through the private `loom editor-session`
length-prefixed UTF-8 transport; each is also available as a one-shot command.
Formatting remains a separate process:

```text
loom editor-check PACKAGE --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
loom editor-query PACKAGE --at ORIGINAL UTF8_BYTE_OFFSET --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
loom editor-complete PACKAGE --at ORIGINAL UTF8_BYTE_OFFSET --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
loom editor-references PACKAGE --at ORIGINAL UTF8_BYTE_OFFSET --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
loom editor-rename PACKAGE --at ORIGINAL UTF8_BYTE_OFFSET --to NEW_NAME --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
loom editor-auto-import PACKAGE --at ORIGINAL UTF8_BYTE_OFFSET --tests [--std STD] [--overlay ORIGINAL SNAPSHOT]...
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
Auto-import output adds `"autoImport": null | { "path", "start": byteOffset,
"text": "import ...\\n" }`; the server turns it into a workspace edit.

The client/server use Microsoft's [Language Server SDK](https://github.com/microsoft/vscode-languageserver-node)
and follow the [VS Code extension guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide).
The TextMate grammar is only lexical highlighting, following the
[syntax highlighting guide](https://code.visualstudio.com/api/language-extensions/syntax-highlight-guide).
