# File-counting programming trial

A small command-line application with a directory package, Unicode text,
file I/O, error handling, records, loops, and same-directory tests. No `src/`
directory or generated project files are needed.

After building the compiler, run from this directory:

```sh
../../../target/loom fmt --check --recursive .
../../../target/loom check
../../../target/loom test
../../../target/loom test stats
../../../target/loom run -- sample.txt
```

The tests report **1** and **2** passes. The program prints `2 4 23`: LF
characters, Unicode-whitespace-separated words, and UTF-8 bytes. `loom test`
selects one directory package; the application's command does not silently
run its imported library's tests.
The library demonstrates both test locations: `empty_input` lives in
`stats/stats.loom`, and the Unicode test lives in `stats/stats_test.loom`.
Both run with `loom test stats`; neither is included when building the application
or the ordinary library. Tests can access private names in their own package.

Try these edits:

1. Add `again` as a third line to `sample.txt`, ending it with a newline.
   Running again should print `3 5 29`.
2. In `stats/stats.loom`, change `var words = 0` to `var words = false`.
   Check the diagnostic, then undo the change.
3. In `stats/stats_test.loom`, change the expected word count from `3` to `4`.
   Run `../../../target/loom test stats` to see a real failing test, then restore `3`.
4. Compact the fields in `Summary`, then run `../../../target/loom fmt stats` (or save in
   the development extension). Formatting restores one field per line.
5. Run `../../../target/loom run -- missing.txt` to check the file-error path.
6. In the editor, add `fn scratch() Int { true }` to `main.loom`. Check that
   the error appears but hover and navigation in `main` still work, then remove it.

The missing-file command exits with `1`; omitting the file argument prints usage
and exits with `2`. `loom run` preserves these application exit codes, just like
running the built executable directly.

Then write a small feature, rather than only running the example: add a `letters`
field to `Summary` that counts ASCII letters (`A`–`Z` and `a`–`z`), update the loop
and constructor, and show it as a fourth column in `display`. Add a same-package
test for `"Go 42!"` (two letters), update existing output expectations, and run
both packages' tests. The sample's new output should be `2 4 23 13`. This deliberately
counts only ASCII letters, not all Unicode letters. Keep the feature if useful;
it is not part of the starting example.

During the exercise, note where you had to guess syntax or look up an API,
whether diagnostics pointed to the mistake, and whether formatting or navigation
interrupted your work. Completion, rename, and automatic discovery of dependency
tests are not available yet; a passing demo is not a claim that these exist.

For a standalone executable:

```sh
../../../target/loom build --output target/wordcount
./target/wordcount sample.txt
```

The development compiler locates its `std` and native tool from its checkout,
not this application's working directory. For a custom toolchain layout, pass
explicit `--std` and `--native-tool` paths. The compiler does not yet provide
`loom init`; the small manifest and source files here are the complete project.

See the [VS Code trial](../../../editors/vscode/README.md#try-it) for unsaved
diagnostics, formatting, hover, and definition navigation. Completion and
rename are not implemented. Independent ordinary functions remain queryable
despite unrelated body errors; syntax and template errors can still block queries.
A failed native assertion reports
the test name and the assertion's original file, line and Unicode column,
including when it fails inside a helper. The test process stops at its first
fault; cleanup runs before the diagnostic is printed.
