# `loom` command-line interface

`loom` is the user-facing project and toolchain command. It drives compilation,
testing, execution, dependency resolution, formatting, publishing, and cache
operations. Unless noted otherwise, commands accept a source file, a directory,
a `loom.toml` path, or a directory containing that manifest. The default path
is the current directory.

Run `loom --help` for the exact syntax supported by the installed binary and
`loom --version` for its toolchain version.

## Synopsis

```text
loom [GLOBAL_OPTIONS] <command> [COMMAND_OPTIONS] [PATH]
```

The commands are:

| Command | Purpose |
| --- | --- |
| `resolve [--update]` | Resolve dependencies and materialize `loom.lock`. |
| `publish --registry NAME` | Publish the root module to an HTTP registry configured in the manifest. |
| `runtime pack --archive FILE --output DIR` | Pack a separately built host runtime archive into a validated bundle. |
| `check [--target NAME]` | Parse, resolve, lower, and type-check the project. |
| `build [--target NAME \| --entry NAME]` | Build an executable, object, interpreted artifact, or portable library. |
| `test` | Compile and execute `test fn` declarations from root-module `*_test.loom` files. |
| `run [--target NAME \| --entry NAME]` | Compile and run a selected exported entry. |
| `run --artifact FILE` | Run an existing native or interpreted executable artifact. |
| `debug [--target NAME \| --entry NAME]` | Build native code with source information and launch a debugger. |
| `fmt [--check]` | Format `.loom` files recursively, or report formatting drift. |
| `cache stat` | Inventory the versioned project compiler cache. |
| `cache prune` | Remove invalid references and unreferenced blobs from that cache. |

## Global options

### Backend and profile

- `--backend llvm|interpreter` selects the backend; `llvm` is the default.
- `--release` selects the LLVM release optimization profile. It is valid only
  for source `check`, `build`, `test`, and `run` commands.

The interpreter does not support `runtime pack`, `debug`, `--release`,
`--target-triple`, or `build --emit object`.

### Dependency resolution

- `--features A,B` enables root-module features.
- `--no-default-features` disables the root module's `default` feature.
- `--locked` requires the resolved graph to match `loom.lock` exactly.
- `--offline` forbids network access and uses only locally available packages
  and validated registry or Git cache entries.

These options apply to `resolve` and source commands. They do not apply to
`fmt`, `publish`, `runtime pack`, or `run --artifact`. `resolve --update`
cannot be combined with `--locked` or `--offline`.

### Compiler cache

- `--no-cache` disables the project compiler cache.
- `--cache-dir DIR` uses a specific compiler cache root.

The two options are mutually exclusive. They apply only to source
`check`/`build`/`test`/`run`/`debug` operations. Registry download caching is a
separate system.

### Machine-readable output

`--json` emits newline-delimited JSON records. Diagnostics, cache events, test
results, tool errors, and the final command result are separate records.
After the invocation has parsed, machine-readable tool errors are written to
standard output. Invalid argument syntax that prevents an invocation from
being formed still prints usage text to standard error. `debug` is interactive
and therefore rejects `--json`.

## Selecting a target or entry

A manifest can declare `bin` and `lib` targets.

- `build` accepts a binary or library target.
- `run` and `debug` accept a binary target.
- `check --target NAME` checks that the named target exists, while the command
  still type-checks the resolved source graph.

If exactly one target of the required kind exists, it is selected
automatically. When several are available, pass `--target NAME`. For a
standalone source file or directory, the default binary entry is `main`.
`--entry` selects an exported function directly and is mutually exclusive with
`--target`. Tests are selected by their file suffix, not a manifest target;
`test --target` is an invalid invocation.

`check`, `build`, `run`, `debug`, library creation, and dependency compilation
exclude `*_test.loom`. `test` adds test files from every package in the selected
root module. It never loads or runs dependency test files.

An executable entry must be a public export in the root module, take no value,
receiver, type, or witness parameters, and return `Unit`. Synchronous and
asynchronous entries use the same selection rules.

## Build outputs

`build` uses these defaults:

| Backend / emission | Default output |
| --- | --- |
| LLVM executable | `target/loom/program` |
| LLVM object | `target/loom/program.o` |
| Interpreter artifact | `target/loom/program.loomi` |

Use `--output FILE` to override the destination.

`build --emit object` produces a relocatable object and does not resolve or
link a runtime. `--target-triple TRIPLE` is valid only for `build`.

Native executable `build`, `test`, `run`, and `debug` resolve a validated
runtime bundle in strict precedence order: `--runtime-bundle DIR`, then
`LOOM_RUNTIME_BUNDLE`, then `runtime/` beside the canonicalized `loom`
executable. An invalid higher-precedence bundle fails closed; it does not fall
through to another source. The host linker is selected by `--linker PROGRAM`,
then `LOOM_CC`, then `clang`.

A non-host executable additionally requires an explicit `--linker PROGRAM`;
`LOOM_CC` is intentionally insufficient at that boundary. The selected bundle
must match the emitted target triple and data layout. Runtime and linker
options are not accepted by `check`, object emission, portable libraries, the
interpreter, or `run --artifact`.

`runtime pack --archive FILE --output DIR` does not compile the archive. It
copies one bounded regular input file to the canonical host archive name,
writes the exact target/runtime ABI/checksum/link-closure manifest, validates
the completed staging directory, and publishes a new destination directory.
It rejects symlinks, oversized inputs, unexpected bundle entries, and an
existing output path.

A `lib` target produces a portable source-and-interface module artifact. Use
an explicit `--output NAME.loomlib`; `.loomlib` is the convention, not an
automatically appended extension. Library targets reject `--release`,
`--emit object`, `--target-triple`, and runtime-link options because they do not
produce native code.

## Running and debugging

Program arguments follow a `--` separator:

```sh
loom run --target app . -- first second
loom debug --target app --debugger lldb . -- first second
```

Trailing arguments are accepted only by `run` and `debug`. On macOS, the
default debugger is LLDB; on other supported native hosts it is GDB.

With the default LLVM backend, `run --artifact FILE` executes an existing
native executable. With `--backend interpreter`, it decodes a `.loomi`
executable and uses its validated embedded entry. The command does not accept a
source path or re-resolve an entry.

Native run and test harness lines are exact UTF-8 byte ranges. The compiler
includes a literal LF byte in each complete line and the ABI writes precisely
that length without NUL scanning, delimiter insertion, or C-runtime text-mode
translation; redirected output therefore uses LF on every supported host. A
successful run's `Unit` line and a passed-test line count as successful only if
the complete range is accepted and flushed. A write or flush failure may leave
a visible prefix, so the generated program does not retry. If the program or
test was already failing, its existing nonzero status is preserved without a
second output-error diagnostic. A pure LCIR root still constructs no Loom
runtime or executor merely to print: its native harness may link the ABI 20
output-only `stdout-v1` symbol. On Unix, a closed output pipe is reported as
this ordinary nonzero output failure rather than terminating the generated
entry with `SIGPIPE`.

## Exit status

| Status | Meaning |
| --- | --- |
| `0` | Command succeeded. |
| `1` | Source diagnostics, a failed Loom test, a program failure, or formatting drift. |
| `2` | Invalid invocation, invalid project/configuration, or an unavailable requested stage. |
| `3` | A compiler, interpreter, or native code-generation defect. |

Native program exit and runtime-fault reporting are normalized by the driver;
use `--json` when automation needs structured records rather than human text.
