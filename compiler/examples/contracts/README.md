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

`forward` proves its own result contract from `good`'s verified contract. The
ordinary call still executes at runtime; proof analysis does not replace it.
Direct scalar calls without a postcondition can instead use the bounded pure
helper body. Argument expressions execute once, in source order, before the
callee's result is considered.

`requires` remains a runtime entry check. Change `good(7)` in `main` to `good(0)`
to see its failure. `ensures` must be proved during checking; helpers used only
for the proof, such as `admitted`, do not become runtime call targets.

This proof fragment supports direct scalar calls, not general function-body
verification. Calls without a usable result contract need bounded, nonrecursive
scalar helper bodies; loops and indirect/dynamic calls remain unsupported.
Arithmetic that executed in a body may rely on its runtime overflow check having
passed. Arithmetic written only in `ensures` must still be proved safe.
Unsupported required proofs fail the build.
