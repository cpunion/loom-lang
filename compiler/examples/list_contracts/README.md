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

`entry.loom` proves append against `old(length(values))`, without an explicit
size parameter. Two consecutive calls use different invocation-entry lengths;
the caller's own `old` remains unchanged. Pure helper observations retain their
entry preconditions and arithmetic obligations. No entry snapshot is allocated
at runtime.

`buffers.loom` alternates two buffers through nested reversal loops. Rebinding
List locals, including inline record/tuple fields, preserves only inductively
proved length relations, not handle identity. Loops that combine rebinding with
resizing conservatively forget heap facts before induction. One buffer is the
input: later passes may update it, and the result is not promised to be fresh.

The sorting example proves only its stated length contract. Its concrete order
and element checks are runtime tests, not sortedness/permutation proofs. General
content proofs and element snapshots remain unsupported. No postcondition helper
is emitted into the native program.
