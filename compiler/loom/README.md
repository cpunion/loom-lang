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
target/loom test compiler/std/loom/checking
target/loom test compiler/std/text
```

Stages 0 through 3 are [bootstrap generations](../../ROADMAP.md#n1--move-the-compiler-into-loom),
not language versions or additional supported compilers. Stage 0 is an existing
Loom compiler, recovered from frozen history on macOS/Linux when needed. The
[Windows bootstrap](../README.md#windows-bootstrap) can instead compile a trusted
same-checkout checked export into its initial native compiler. New language
features do not require a parallel Rust implementation. Keep the compiler and
its production `std` dependencies within the existing seed's supported subset,
even when user programs can use newer features. Raise that requirement only for
substantial simplification or measured performance gains, batching upgrades to
avoid a growing checkpoint chain. Test fixtures may exercise newer syntax.
Source Map adoption, for example, compiles with the existing pinned seed and
does not add a bootstrap checkpoint. Use one-stage `--dev` for ordinary edits;
the three-stage comparison is the bootstrap/CI validation gate.

The source CLI provides `check`, `build`, `test`, and `run` for one directory
package. When invoked by path, a development compiler locates `compiler/std`
and `target/debug/loom-native` in its own checkout, independently of the working
directory. Use `--std` and `--native-tool` for other layouts. `build` accepts
`--output`; native commands also accept `--emit-ir`. Library builds produce an
object, and production excludes test files and test declarations.
`run [package] -- [arguments...]` forwards arguments verbatim to the program;
relative file arguments remain relative to the caller's working directory.
`--object-cache` enables [trusted-local object reuse](../README.md#native-object-cache)
for native commands. The Loom driver owns cache policy; the Rust bridge only
identifies its implementation, emits objects and links/publishes requested files.
Source checking always runs and executables always relink.
The private `emit-checked` command performs normal source/type/proof checks but
writes the checked artifact to stdout without invoking a native tool. It serves
bootstrap transfer, not a stable interchange or cache format.

`lex` and `parse` inspect one or more source files, reporting token/declaration
counts or positioned diagnostics. For example:

```sh
target/loom parse compiler/loom/main.loom
target/loom test compiler/std/loom/proof
LOOM_GC_STRESS=1 compiler/std/loom/proof/target/tests
```

The bootstrap integration gate compares stage 2 and 3 binaries, selected
diagnostics and executable results, then runs compiler and source `std` tests.
Agreement is evidence, not a proof of compiler correctness. Native bootstrap and
tests pass on macOS, Linux and Windows through the same LLVM 22 bridge.
Manifest helpers still implement the documented subset, not general TOML.

## Formatting and editor feedback

```sh
target/loom fmt path/to/package
target/loom fmt --check --recursive path/to/module
target/loom fmt --stdin < main.loom
```

`fmt` rewrites `.loom` files in the selected directory, or explicit source files;
the default path is `.`. `--recursive` includes nested directories. `--check`
does not write and fails when formatting is needed. `--stdin` formats supplied
UTF-8 source to stdout without touching files. Formatting preserves AST structure,
comments, and literal spelling, with four-space indentation and one field or
statement per line. It separates top-level declarations and retains at most one
user blank line. This changes layout, not grammar; semicolons remain invalid.

The [VS Code development extension](../../editors/vscode/README.md) provides
highlighting, document formatting, diagnostics, name/member completion, type hovers and definition
navigation for unsaved buffers. Its
language server sends source snapshots to the same Loom package/type/contract
checker; it does not implement another parser or checker in JavaScript.
Name completion uses package bindings and lexical scopes without type checking;
member completion infers the receiver through ordinary signature/body-prefix
checking. Nested tuple bindings contribute ordinary names and receiver types.
It offers fields, tuple indices, admitted concept methods and async
Task `.await`, not proof of an applicable call or valid body. Rename and incremental
semantic caching remain open. Completion
can recover a missing cursor name/value or unmatched EOF delimiters
without modifying the source or making normal builds accept it. Other semantic
queries use concrete body instances. If package checking fails, an independently
checked ordinary function can still provide hover and navigation; its own errors
or a failing dependency suppress the result. Syntax, global declaration, and
generic/comptime template errors can still block queries.
An isolated macOS VS Code extension-host test covers activation, unsaved errors,
error clearing, name/member completion and applied formatting. The
[file-tool trial](../examples/wordcount/README.md) exercises a multi-package
application from its own directory. Broader interactive usability review remains open.

Package-qualified completion enumerates current-package and explicitly imported
names, preserving overloads, private/test scopes and dependency instance identity.
Local receivers do not fall back to namespaces. Type/constructor paths offer
types and `dyn` paths offer concepts, without hiding a same-named type parameter.
Import-path discovery and automatic imports remain open.

## Public syntax libraries

The compiler and ordinary Loom programs use the same source implementation:

| Package | Public entry points and data |
| --- | --- |
| `std.loom.source` | `Span`, `Diagnostic`, `Position`, `position`, `render` |
| `std.loom.lexer` | `lex`, `Token`, `Kind` |
| `std.loom.ast` | `Node`, `NodeKind`, `has` (direct-child lookup), `same` (exact structural equality) |
| `std.loom.parser` | `parse(Text) Result[Node, Diagnostic]`, editor-only `completion_source` / `CompletionSource` |
| `std.loom.format` | `format(Text) Result[Text, Diagnostic]` |

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
`ast.same` includes spans as well as node values and children; it is not an
identity-aware or formatting-preserving comparison.

This is an evolving public API, not a stable node schema or lossless editor
tree: comments and formatting trivia are discarded, and string token values are
decoded. Preserve original source when tooling needs its spelling and layout.
The formatter uses that original text alongside token and AST spans to preserve
comments and literal spelling; it does not require a separate lossless AST.
Typed analysis is an opt-in layer below. Typed metaprogramming and identity-aware
editing remain later library boundaries in the [roadmap](../../ROADMAP.md).

## Public project and binding libraries

Project analysis is opt-in; in-memory syntax users do not import these layers:

- `std.loom.manifest.parse(text)` returns `Result[Module, Text]`, with module
  name/version metadata and `dependencies List[Dependency]` (`name`, `source`).
  `DependencySource` is `Path(Text)` or `Git(Text, Text)` (URL, full commit ID). It
  parses the supported manifest subset, not general TOML or source resolution.
- `std.loom.project.load(path, std_root, tests)` returns `Result[Project, Text]`.
  `Project` contains files, module instances, packages and a root package ID. It reads
  the selected directory package and its import closure, not the whole
  repository. Only the selected root contributes tests when requested.
  Manifest-relative path dependencies resolve from each importing module's
  direct entries. Canonical roots are reused; distinct roots with the same name
  keep separate package and nominal type identities.
- `std.loom.project.resolve(path, std_root, tests, git_tool)` uses that same
  selected-package traversal, fetching exact Git sources and publishing the
  root module's lock only after successful loading. Ordinary `load` is offline
  and verifies selected cached snapshots against their locked contents and names.
  A compile-time mode shares traversal without retaining fetch/write operations
  in native consumers that use only `load`.
  See [module dependencies](../README.md#module-dependencies) for source policies
  and the remaining resolver limits.
- `std.loom.binding.bind(project, tests)` returns `Result[Program, Failure]`
  after declaration/import validation. Inspect `Program.symbols` for
  declarations; `Failure.source` identifies the input file for its diagnostic.

`SourceFile.package` indexes the project's package table. Each package records
its module instance, relative directory and resolved import edges. Module source
identities are canonical roots for path dependencies. `package_label` provides
display names, not unique identifiers. In-memory consumers can construct the
same graph without loading files; these IDs belong to that project basis.

Package identities follow directories. Root and `std` paths are canonicalized;
imported directory symlink aliases that change package identity are rejected.
Source-file symlinks remain allowed, with trust based on canonical file paths.
`join_path` preserves native separators when appending a component to a canonical
absolute path; `is_windows_path` classifies its spelling, not the running host.
Drive, UNC, and Windows verbatim prefixes are preserved. Unix backslashes are
not rewritten as separators.

`candidates(program, file, test_only, path)` returns indices into that program's
symbol table, respecting the supplied file's visibility and test context.
These are name/overload candidates, not the selected function at a call site;
binding does not type-check expressions or prove contracts. `Symbol.file`
indexes `Program.files`, and `Symbol.node.span` locates the declaration there.
Package scopes use source `std.map` for name lookup, with separate production
and test maps. Candidate lists preserve declaration order and are copied on
return; changing a returned list cannot change later lookups. Hash-table order
never determines overload precedence.
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

## Public typed analysis

`std.loom.analysis` uses the compiler's binder, checker, and required prover
without the CLI, LLVM backend, or filesystem loading:

| Function | Result |
| --- | --- |
| `analyze(project, tests)` | `Result[Analysis, Failure]`, with bindings and a checked program |
| `type_name(analysis, ty)` | Display name for a type in that analysis |
| `expressions_at(analysis, file, offset)` | Smallest covering expression per concrete function instance |
| `inspect_at(analysis, file, offset)` | `Option[Inspection]`: token span, checked type/signature labels and definition locations |
| `inspect_independent(bindings, file, offset)` | Isolated inspection of a concrete function and its checked dependencies; no executable program |
| `complete_names(bindings, file, offset)` | `Option[Completion]`: visible names and declaration signatures, using binding only |
| `complete_at(bindings, file, offset)` | Names, qualified paths or receiver-type member hints, with a UTF-8 replacement span |
| `is_current(analysis, project, tests)` | Whether the supplied project still matches the snapshot |

The [semantic example](../examples/semantic/main.loom) creates an in-memory
project, queries inferred local types and selected overload/generic call targets,
and detects a changed source snapshot:

```sh
target/loom test compiler/examples/semantic
target/loom run compiler/examples/semantic
```

`Analysis` owns a detached project/AST snapshot; treat its returned lists as
read-only. `is_current` compares module origins, package/import edges, file
metadata, text and AST, not the disk: reload first to detect filesystem changes.
These are result-local indices, not persistent identities or an incremental cache.

Query offsets are half-open UTF-8 byte offsets. Equal spans prefer the outer
resolved expression, including field selections and coercions. A call's index
addresses `analysis.program.functions`; local indices address that function's
locals. Uninstantiated generic templates and signature-only positions have no
typed result. The evolving checked model is not a stable public schema or a
typed macro API.

Completion candidates are not checked expression results. Member queries reuse
the ordinary parameter/match binders, preceding statements and explicit concept
evidence; they do not check the unfinished body or prove its callees/contracts.
Unknown receivers, earlier typing errors, undetermined compile-time branches and
`comptime` execution blocks return no receiver result. Qualified type spelling
queries need only bindings; expression paths retain the enclosing prefix checks.
No import discovery or automatic imports are provided. The caller supplies matching source/AST bindings;
CLI-only cursor recovery maps virtual insertion ranges back to the original buffer.

`inspect_at` selects the source token's role, so a receiver and its selected field
have distinct answers even when lowering shares their spans. `Inspection.types`
and `.definitions` retain deduplicated answers across concrete instances;
each `Location.file` indexes the analysis files, with an identifier byte span.
Local navigation follows checked local IDs, including shadowing. Calls use
resolved targets; dynamic calls point only to an identifiable concept signature,
never a guessed implementation. Comments, punctuation and unavailable semantic
identity return no answer. Match bindings currently have hover types only.

`inspect_independent` retains the complete bindings and starts a fresh checker
session; it does not remove erroneous declarations or reuse a failed check's
tables. Global declarations and generic/comptime templates must still pass.
Only a containing non-generic, non-comptime-parameter function can be selected.
Its concrete dependency closure, contracts and resource flows must pass before
any facts are returned. Normal `analyze`, `check` and `build` remain whole-package
checks. Query tables are a separate `CheckedQuery`, not a partial `typed.Program`.

`Type.constraint` is predicate-template metadata, not executable IR: `self`
uses local 0 and call indices are `-1` until rebound at a construction boundary.
Do not interpret those indices as references into `program.functions`.

Function types store parameter type IDs followed by the result ID in
`Type.arguments`. A `FunctionRef` expression names one concrete function instance;
its optional child supplies the typed managed environment of a lifted closure.
an `IndirectCall` stores its callee before the arguments in `Expr.children`.
The callee has no single statically selected call index. Compile-time function
values retain source symbols and type arguments for destination reification.

## Compiler packages

Shared `std.loom.typed`, `checking`, and `proof` packages implement the checked
model, concrete specialization, and bounded required proofs. `std.loom.eval`
evaluates pure checked expressions for
[compile-time execution](../README.md#compile-time-execution), not as a second
runtime backend. Unsupported required proofs still reject the build.
Its `validate` checks the supported operation/call closure in already checked
IR; callers must establish input isolation. It is not a general effect proof
for functions receiving externally shared mutable data.

The remaining `frontend` packages are:

- `artifact`: a private counted UTF-8 stream to the LLVM tool, not a stable
  package, cache or public AST format. Proof-only locals are not emitted.
- Root: CLI orchestration using source `std.fs`, `std.file`, `std.io` and
  `std.process`. Process arguments are literal; no shell is implicitly invoked.

This is the self-hosting subset, not the complete accepted language. Broader
contracts, concepts, variadics/typed macros, resources, Tasks and module
resolution remain in [implementation status](../../docs/project/implementation-status.md).
