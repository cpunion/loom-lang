# Reusable contract predicates

From the repository root:

```sh
target/loom check compiler/examples/contracts
target/loom test compiler/examples/contracts
target/loom run compiler/examples/contracts
```

`good` proves its postcondition by checking the body of `admitted`, including
that helper's own requirement. Remove `requires positive(value)` from `good`:
the compiler must reject it, even if every current caller passes a positive
constant. `bounded` reuses a predicate with multiple parameters and an immutable
local.

`requires` remains a runtime entry check. Change `good(7)` in `main` to `good(0)`
to see its failure. `ensures` must be proved during checking; helpers used only
for the proof, such as `admitted`, do not become runtime call targets.

This proof fragment supports bounded, nonrecursive scalar helpers. It does not
prove loops or indirect/dynamic helper calls, or general calls in the function
body being verified. Unsupported required proofs fail the build.
