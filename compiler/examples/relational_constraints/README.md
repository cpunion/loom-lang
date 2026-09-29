# Relational construction checks

```sh
target/loom check compiler/examples/relational_constraints
target/loom test compiler/examples/relational_constraints
target/loom run compiler/examples/relational_constraints
```

`above` combines two parameter bounds. `next_percent` relates an immutable
initializer to its result. `retained` keeps a scalar copy's identity even when
evidence arrives after the copy. `guarded` uses the sole continuing branch.
These constructors return the constrained value directly: their repeated checks
are absent before LLVM optimization. The original assertions, requirements and
initializer overflow checks still execute.

`unresolved` cannot assume either side of a disjunction; it keeps checked
`Result` construction. The bounded proof supports integer difference relations
and scalar linear expressions, not arbitrary arithmetic, mutable bindings,
shared contents or merging facts from two live branches. Float predicates are
reused exactly, without integer algebra or NaN-equality assumptions.
