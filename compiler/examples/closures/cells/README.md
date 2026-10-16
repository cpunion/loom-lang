# Captured storage contracts

```sh
target/loom check compiler/examples/closures/cells
target/loom test compiler/examples/closures/cells
target/loom run compiler/examples/closures/cells
target/loom test compiler/examples/closures/cells/loops
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

The loop package uses the same inductive rules as List elements. A captured
counter advances to a requested bound; entry and every backedge must establish
the candidate invariant. Cell stores participate in helper write footprints,
so unchanged lengths never imply unchanged contents. Shared interference still
prevents treating separate counter reads as one value.
