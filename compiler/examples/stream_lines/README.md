# Scoped async line pipeline

```sh
target/loom check compiler/examples/stream_lines
target/loom test compiler/examples/stream_lines
target/loom run compiler/examples/stream_lines -- README.md
```

Infallible factories transfer a lazy file-line stream through `filter` and `take`.
The final owner enters `scoped`; async predicates and `try_fold` borrow it only
through directly awaited calls. The first two nonempty lines are returned,
without decoding a later suffix. UTF-8/open errors propagate normally.
The first pull opens the file; bounded native workers perform open/read/normal
close without blocking the scheduler thread. Cancellation drains running I/O and
suspended callbacks before fallback file cleanup. Construction and zero-item
limits do not open a file. No whole-file buffer is retained.
