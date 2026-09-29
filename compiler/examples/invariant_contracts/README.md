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

This proof slice consumes call-free integer/Boolean predicates over immutable
scalar and inline record values, including refined leaves inside tuples.
Unsupported conjuncts supply no evidence; disjunction alternatives are never
assumed individually. Helper calls in invariant templates, shared contents and
required Float arithmetic are not added to the proof fragment. Predicates and
inline traversal use the existing proof budgets; unknown required proofs reject.

The native program retains the original construction checks. Entry invariant
facts are compile-time evidence, not additional runtime checks or a new ABI.
