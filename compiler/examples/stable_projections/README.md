# Stable projections, mutable contents

```sh
target/loom check compiler/examples/stable_projections
target/loom test compiler/examples/stable_projections
target/loom run compiler/examples/stable_projections
```

The table predicate fixes its first row's key, not its quantity. A Snapshot can
constrain its inline key to that shared projection while aliases replace the
quantity, including during real suspension and worker execution.
The prefix example also permits appending: neither the whole contents nor the
length must be fixed when the observed projection remains proved stable.

The compiler proves the parent predicate again after arbitrary legal shared
changes, including bounds and other evaluation obligations. It does not freeze
the collection, copy it, or infer stability from the application's sample calls.
Each actual store independently preserves the complete child predicate.

Changing the parent to observe quantity rejects: that projection is mutable.
Unknown proofs reject too. This is bounded predicate revalidation, not general
induction or a promise that separate shared reads return the same value.
