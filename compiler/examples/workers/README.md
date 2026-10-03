# Shared CPU workers

```sh
target/loom check compiler/examples/workers
target/loom build compiler/examples/workers
target/loom run compiler/examples/workers
target/loom test compiler/std/task/worker
```

The two callbacks share a counter and mutex, then compute outside the critical
section. Their distinct typed results join through ordinary Task tuple awaiting.
The package tests also force a lost update without compound synchronization,
exercise mutable captures and aggregate publication during moving collection,
and check cancellation/fault cleanup. See the [worker guide](../../README.md#shared-workers)
for the memory, proof and cancellation boundaries.

## Local storage

`local.loom` constructs private List/Bytes buffers inside a worker and proves
the length and indexed contents of a fresh result. Private accesses omit
shared-storage locks; GC roots and cancellation checkpoints remain. This needs
no ownership annotations and does not change the shared-counter example.

`observed_snapshot` passes a shared length to a pure helper. Arguments and local
bindings are evaluated once, so the helper can reuse that scalar snapshot.
Writing `length(values) == length(values)` instead makes two independent reads
and cannot prove equality under interference. The same distinction applies to
`old` observations; a snapshot does not freeze the underlying List.
