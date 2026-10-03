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
