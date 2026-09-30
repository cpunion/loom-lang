# Inferred scalar loop contracts

```sh
target/loom check compiler/examples/loop_contracts
target/loom build compiler/examples/loop_contracts
target/loom test compiler/examples/loop_contracts
target/loom run compiler/examples/loop_contracts
```

The compiler proves the declared normal-return contracts by induction, not by
testing a finite number of iterations. It proposes scalar entry/range bounds and
weakened continuation bounds, then retains only predicates established on entry
and preserved by every symbolic backedge. Removing one candidate triggers a
recheck of the others. Zero-iteration exits are checked separately.

The current subset includes Int/Bool local assignments, branches, early returns
and nested loops. Direct body calls reuse verified summaries or finite pure
expansion. Unsupported effects, calls in guards, cleanup and break/continue
reject in a required proof. Unknown proofs remain build errors;
there are no generated loop-invariant checks, termination promises or proofs
that arithmetic cannot fault. List sorting/permutation proofs remain separate
work. Ordinary loops without `ensures` are not restricted by this proof subset.
