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

`refinements.loom` uses `Nonempty[T]` with shared nested Lists. Its predicate
observes only the outer length, so inner mutations do not invalidate it. Outer
replacement and append preserve nonemptiness through generic helpers and
suspension. Fresh construction proves a singleton directly; copying unknown
input retains checked `Result` construction, including rejection of empty input.
The copy shares nested elements but not the outer header: resizing the input
does not change its length, while nested updates remain visible through a
non-escaping read helper. Generic `first` also returns the shared inner List:
its stored type cannot contain the protected outer header, so the caller may
mutate it without invalidating the outer extent.
The account example constrains each immutable `amount` beside shared `notes`.
Input replacement and notes updates cannot change the copied amounts, including
across suspension; a nonpositive account fails checked construction. Reading
shared notes in the predicate rejects rather than assuming they remain unchanged.
Raw writable outer aliases still cannot escape. The factory must prove fresh, unpublished
outer storage; purity or a method named `clone` is not sufficient.
Potential recursive backreferences and opaque function/dyn captures cannot escape
through this type-graph rule.

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
content proofs remain unsupported; bounded indexed reads/writes and immutable
entry-element snapshots have their own [example](../list_elements). No
postcondition helper is emitted into the native program.
