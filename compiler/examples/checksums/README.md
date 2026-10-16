# Concurrent file checksums

A small native application combining directory packages, constrained input,
required contracts, scoped file streams and dynamically sized Task groups.
It computes SHA-256 over arbitrary binary files without retaining whole files.

From the repository root:

```sh
target/loom fmt --check --recursive compiler/examples/checksums
target/loom check compiler/examples/checksums
target/loom test compiler/examples/checksums
target/loom test compiler/examples/checksums/files
target/loom test compiler/examples/checksums --recursive
target/loom run compiler/examples/checksums -- --chunk-size 7 compiler/examples/wordcount/sample.txt
target/loom build compiler/examples/checksums --output target/checksums
target/checksums compiler/examples/wordcount/sample.txt compiler/examples/checksums/main.loom
```

The main package reports **3 tests passed**; `files` reports **2 tests passed**.
Recursive testing selects both private test scopes. The files package contains
both an embedded `test fn` and a separate `files_test.loom`; neither is included
when compiling it as an ordinary library or building the application. Its I/O
test creates an exclusive `loom-checksums-test` directory below the current
working directory and cleans it up; an existing directory is never overwritten.

## Read the program

- [options.loom](options.loom) checks dynamic chunk sizes once at construction.
  `ChunkSize` admits 1 through 1048576 bytes; the known default constructs a
  direct constrained value, not a Result. No `src/` directory is needed.
- [files/files.loom](files/files.loom) exports only `ChunkSize` and `checksum`.
  It owns a `scoped` lazy file stream, updates an incremental source-written
  hasher, and propagates open/read/close errors. GC never closes the file.
- [main.loom](main.loom) builds batches of at most four Tasks and collects each
  batch in input order. `batch_end` statically proves its boundary contract,
  including defined arithmetic near the integer limit. It is not a proof of
  SHA-256 correctness, file stability or all application behavior.

Task-bearing Lists use `std.list.transfer.append` and save its returned header.
Ordinary shared `push` does not transfer the one-shot container back to the
caller. This is an ordinary library operation and assignment, not ownership
syntax or a copied List. All pending Tasks are consumed by `std.task.all`.

File I/O can overlap; hashing runs on the scheduler's owner thread, not an
implicit CPU worker. At most four file Tasks hold bounded chunks and fixed hash
scratch; allocator, GC and native I/O overhead is additional. Paths and 64-digit
results remain proportional to the argument count. Concurrent external file
writers need their own coordination; reads do
not promise a filesystem snapshot.

The output is `DIGEST`, two spaces and the original path, one line per successful
input. A file error is reported on stderr, other inputs still finish, and the
exit code is **1**. Missing arguments or invalid chunk sizes exit with **2**.
Otherwise the exit code is **0**. The program reads input files; it does not
modify them. Newline-containing names have no escaped output format.

## Try a change

Open `main.loom` and `files/files.loom` in the development VS Code extension.
Hover `limit` and navigate from the imported `checksum` call to its declaration.
Change `result - offset <= 4` to `<= 3`: `loom check` must reject the required
postcondition, rather than substituting a runtime test. Restore `4` afterward.
Try `--chunk-size 0` or add a missing path to observe the two distinct error
paths. Use several files to exercise the final partially filled batch.

The native integration test compares complete digests against an independent
implementation across two batches, Unicode/space-containing paths, empty and
binary files, missing input, invalid configuration and forced moving GC.
