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
remain one-shot. `NoSuspend` and `MustScope` parameters are rejected for async
methods; scoped receivers cannot escape into their child Tasks. This does not
yet provide structured async borrowing or Task-bearing aggregates/function values.
