# Stateful List length contracts

```sh
target/loom check compiler/examples/list_contracts
target/loom test compiler/examples/list_contracts
target/loom run compiler/examples/list_contracts
```

The required proofs cover generic length helpers, exact local aliases, append,
growth loops and length preservation during indexed reads/writes. Arguments run
once in source order; bounds and overflow checks retain ordinary fault behavior.
Unknown alias overlap or an opaque call discards affected current-length facts,
while a previously read scalar length remains an immutable snapshot.

The generic copy example also proves its output length. Fresh outer allocations
are distinct from earlier handles; inner elements retain sharing. The loop's
output-length/index equality is checked inductively, including zero iterations.
Later unknown handles and opaque calls can still reach those allocations.

The sorting example proves only its stated length contract. Its concrete order
and element checks are runtime tests, not sortedness/permutation proofs. General
content proofs, mutable `old` snapshots and List-local reassignment in proof loops
remain unsupported. No postcondition helper is emitted into the native program.
