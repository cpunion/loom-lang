# Captured storage contracts

```sh
target/loom check compiler/examples/closures/cells
target/loom test compiler/examples/closures/cells
target/loom run compiler/examples/closures/cells
target/loom test compiler/examples/closures/cells/workers
```

Closure copies share mutable captured bindings. Contracts follow the actual
cell, including writes through another callback, and `old(value)` observes its
entry value. Rebinding a constrained List handle retains the new value's type
guarantees; it neither copies nor freezes either List.

The worker package is separate because its build uses interference rules.
`Positive` holds for every stored value, but two reads of a mutable Int cell need
not be equal. Unknown callback effects invalidate observations. These are static
proofs using the ordinary storage algebra, not runtime contract monitors.
