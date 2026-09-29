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
assumed individually. Helper loops/recursion, shared contents and required Float
arithmetic remain outside this fragment. Unknown required proofs reject.

The native program retains the original construction checks. Entry invariant
facts are compile-time evidence, not additional runtime checks or a new ABI.
