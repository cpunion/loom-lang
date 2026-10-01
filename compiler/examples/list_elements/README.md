# Indexed List contracts

```sh
target/loom check compiler/examples/list_elements
target/loom test compiler/examples/list_elements
target/loom run compiler/examples/list_elements
```

The program proves indexed writes, swaps with and without returned values,
literal/append reads and contract composition across calls. Int, Bool, Text and supported inline
element values retain established observations. Exact handle aliases share
them; another index survives a write only with proved inequality, and another
handle only with proved allocation separation.

Entry checks execute normally. Postcondition indexing must separately prove
bounds; it cannot assume a hypothetical runtime check passed. Opaque calls
forget element observations even if lengths survive.
Scalar values already read remain snapshots.

`loops.loom` proves a nondecreasing element and conservation of two elements'
sum while transferring values between them. Mutating loop heads use fresh values;
element bounds and source-contract candidates must survive entry and backedge
checks. Failed candidates are removed and their dependents rechecked. The native
trial also checks `continue`, `break`, and return-value snapshots with block
cleanup. Length-changing loops and unknown alias writes remain conservative.

`old(values[index])` and pure helper reads snapshot immutable element values
before the body. Bounds must be proved at entry, even under a conditional
postcondition. Consecutive calls each use their own entry, not stale contents
from a previous invocation. No mutable element handles are snapshotted.

The bounded model retains at most 64 observations per handle. Unknown elements
have arbitrary values; a postcondition needs an established read or write at its
index. It does not prove general array extensionality or sorting/permutation.
No runtime List metadata or postcondition checks are
added, and the contract-only helper is absent from native code.
