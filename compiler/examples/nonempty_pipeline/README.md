# Nonempty collection pipeline

Run from the repository root:

```sh
target/loom check compiler/examples/nonempty_pipeline
target/loom build compiler/examples/nonempty_pipeline --output target/nonempty-pipeline
target/loom test compiler/examples/nonempty_pipeline
target/loom run compiler/examples/nonempty_pipeline
```

An external List crosses `copy_from`'s checked boundary once. Generic `map`
returns `NonEmpty[U]` directly; a later generic stage constructs a positive count
without another runtime predicate check. `to_list` explicitly copies the outer
header for ordinary List consumers rather than exposing a resizable alias.

The tests cover compile-time execution and shared workers. Concurrent appends
can change how many elements a worker initially observes; only nonemptiness is
promised, not equality with a separate mutable length read.
