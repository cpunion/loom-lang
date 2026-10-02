# Lazy source iterators

```sh
target/loom check compiler/examples/iterators
target/loom test compiler/examples/iterators
target/loom run compiler/examples/iterators
```

The program composes List and range iterators, associated types, lazy callbacks
and dynamic consumers. It checks shared element updates, early stopping, ordered
cleanup after a callback fault and resumption of the unconsumed source.
Native integration runs it at O0/O2 with moving-GC stress.
See [`std.iter`](../../std/iter/README.md) for the library's consumption rules.
