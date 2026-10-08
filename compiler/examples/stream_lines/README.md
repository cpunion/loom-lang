# Scoped async line pipeline

```sh
target/loom check compiler/examples/stream_lines
target/loom test compiler/examples/stream_lines
target/loom run compiler/examples/stream_lines -- README.md
```

Factories transfer a file-line cursor through `from_iter`, `filter` and `take`.
The final owner enters `scoped`; async predicates and `try_fold` borrow it only
through directly awaited calls. The first two nonempty lines are returned,
without decoding a later suffix. UTF-8/open errors propagate normally.
File reads themselves are synchronous: lifting an Iterator does not turn its I/O
into reactor I/O. Cancellation drains suspended callbacks before closing the file.
