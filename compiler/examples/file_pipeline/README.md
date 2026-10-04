# Scoped file pipeline

```sh
target/loom check compiler/examples/file_pipeline
target/loom test compiler/examples/file_pipeline
target/loom run compiler/examples/file_pipeline -- compiler/examples/file_pipeline/main.loom
```

The application prints the first two nonempty UTF-8 lines, then closes the file
without consuming the suffix. Factory overloads of filter/take create one owned
pipeline; try_fold propagates error items rather than filtering them away.
Open/read/decode/close errors produce a nonzero exit status.

This uses ordinary functions, Result and nested `scoped` disposal, not a runtime
execution graph or ownership syntax. See [`std.iter`](../../std/iter/README.md).
