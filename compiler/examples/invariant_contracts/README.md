# Input invariants in function contracts

```sh
target/loom check compiler/examples/invariant_contracts
target/loom test compiler/examples/invariant_contracts
target/loom run compiler/examples/invariant_contracts
```

`Positive` and `Ordered` establish their invariants at construction. Functions
receiving those values can prove postconditions without repeating their type
constraints as `requires` clauses. Nested inline fields keep their own facts,
even beside an unrelated shared List. Failed construction still returns `Result`.

This proof slice consumes integer/Boolean predicates over immutable scalar and
inline record values, including refined leaves inside tuples. The predicates
here call ordinary pure functions. When direct facts are insufficient, bounded
symbolic expansion retains their results, preconditions and successful checked
calculations, guarded by the branches that actually executed at construction.
These proof-only helpers do not become new runtime roots.

Unsupported conjuncts supply no evidence; disjunction alternatives are never
assumed individually. General helper loops/recursion and floating-point algebra
remain outside this fragment. Exact IEEE comparisons use their separate value
rules; see the [Float contract trial](../floats/contracts.loom).
Unknown required proofs reject.

`lists.loom` extends entry evidence to bounded, guarded List predicates, nested
fields and `old(values[0])`, without repeating `requires`. Unknown alias writes
invalidate content observations. Shared-worker interference needs separately
validated storage evidence; a length-only invariant does not establish element
values. The [worker example](../workers/constrained.loom) also consumes bounded
sorted-input predicates, without treating arbitrary loops as proved.

The native program retains the original construction checks. Entry invariant
facts are compile-time evidence, not additional runtime checks or a new ABI.
