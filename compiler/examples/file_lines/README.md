# Incremental file lines

```sh
target/loom check compiler/examples/file_lines
target/loom test compiler/examples/file_lines
target/loom run compiler/examples/file_lines -- compiler/examples/file_lines/main.loom
```

The program prints the line count of a UTF-8 file without retaining all its
contents. Missing files, invalid UTF-8 and read/close failures produce a nonzero
exit status. The scoped file closes on normal EOF, early return and faults.
Line endings follow [`std.file.lines`](../../std/file/lines/README.md).

Native integration covers O0/O2 and moving GC; colocated source tests exercise
the application and the library's line boundaries, errors and lexical cleanup.
