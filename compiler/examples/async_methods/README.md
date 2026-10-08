# Async methods

```sh
target/loom check compiler/examples/async_methods
target/loom test compiler/examples/async_methods
target/loom run compiler/examples/async_methods
```

The program prints `methods finished`. It calls the same async capability through
a concrete value, a bounded generic parameter and `dyn Source[Item = Int]`.
Default methods can await required methods, specialize type/comptime parameters,
and transfer Tasks through synchronous forwarding or nested async results.

Calls snapshot arguments once and enqueue hot child Tasks; shared fields retain
their ordinary sharing semantics. Dynamic witnesses point to the same typed Task
constructors as static calls, without a separate executor or runtime type search.
Unused methods do not enter a closed executable's reachability.

An implementation must match its concept's `async` modifier. Task inputs/results
remain one-shot. NoSuspend parameters still reject. Scoped receivers can be
borrowed by directly awaited calls: completion or cancellation drains their
children before the owner is disposed. The borrowing Task cannot be stored,
forwarded or returned. See the [scoped stream trial](../stream_lines/README.md).
