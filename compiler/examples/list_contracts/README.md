# Stateful List contracts

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
Construct and validate a new `PositiveAccount` before replacing or appending it
to either `PositiveAccounts` or `Nonempty[PositiveAccount]`; aliases see the replacement, while
shared notes retain ordinary mutation. `first_amount` proves its positive return
from the element invariant, without a duplicate assertion or runtime contract.
Raw writable outer aliases still cannot escape. The factory must prove fresh, unpublished
outer storage; purity or a method named `clone` is not sufficient.
Potential recursive backreferences and opaque function/dyn captures cannot escape
through this type-graph rule.

`projections.loom` uses the same array algebra for nested record and tuple integer
fields. A pure scan binds each row and checks relations between its fields,
then requires nonemptiness. Replacement, append and an ordinary fill loop prove
the complete constraint at every store, without runtime predicate checks.
Unobserved notes remain shared across suspension. An unconstrained inline List
also proves arbitrary-index write/restore contracts for each field; field
equality never implies equality or permutation of whole records.

`scalars.loom` applies that same select/store algebra to Bool, Text and Float
columns, beside shared notes. A mixed constraint proves replacement, append and
loop filling; pure Text-length predicates are imported from the replacement's
type. Text lengths count UTF-8 bytes. Float storage preserves its IEEE value:
restoration proves numeric equality only when NaN has been excluded, and signed
zeros retain their distinct representations. These are compile-time facts, not
runtime copies or a solver dependency in the application.

`snapshots.loom` queries Bool, Text and Float entry columns through nested inline
fields with `old(values)[index]`, even when a row has shared notes. Pure generic
selection helpers retain the chosen entry version, including overlapping inputs.
Returned indices must satisfy entry bounds. Generic append observes only the
outer header, so nested Lists retain ordinary sharing rather than becoming deep
snapshots. Float numeric equality still requires excluding NaN.

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
content induction beyond the supported array/scan rules remains open; bounded indexed reads/writes and immutable
entry-element snapshots have their own [example](../list_elements). No
postcondition helper is emitted into the native program.
