# Immutable Text contracts

```sh
target/loom check compiler/examples/text_contracts
target/loom test compiler/examples/text_contracts
target/loom run compiler/examples/text_contracts
```

The example proves Text identity through finite helpers, nested calls, `old`,
record fields and declared generic/dyn concept summaries. Text fields retain
their values while shared List siblings remain shared. An async function retains
the same immutable value without a runtime postcondition or entry snapshot.
Established equality chains and inequality between equal-value classes compose;
two different symbolic inputs alone never establish inequality.

`Tag("ready")` constructs a Text refinement directly. Unknown construction
returns `Result[Tag, ConstraintError]`; both accepted and rejected inputs run.
The native test checks O0/O2 IR for proof-only helper elimination and runs the
artifacts under moving-GC stress, retaining dynamically allocated Text and a
refined Text value across timer suspension.

The [construction example](construction.loom) uses immutable entry, branch and
copy facts to construct refinements directly, including a pure helper predicate.
The native artifact retains declared entry checks but no extra predicate helper;
mutable variables and unknown disjunction alternatives still need validation.

The [byte-length example](lengths.loom) proves UTF-8 byte counts (six for `é😀`,
not two characters), preserved entry lengths and equal-value lengths. Checked
1–8-byte names widen to a 1–16-byte type without a second check, while unknown
construction still returns a checked Result. Length constraints remain valid
across GC and suspension. Equal lengths do not imply equal contents.
The source `std.text.length`, `concat` and `slice` APIs declare required
normal-return contracts for byte counts and exact contents. Library callers
reuse those guarantees, including direct refined construction, CTFE and workers;
ordinary slice bounds/UTF-8 faults and hypothetical-call safety are unchanged.

The [concatenation example](concatenation.loom) proves additive byte lengths,
empty identity, reassociation, substitution of equal inputs and entry contents.
A length constraint involving a prefix also accepts a proved weakening directly.
The native exercise checks eager left-to-right argument snapshots. These are
bounded symbolic proofs, not sampled strings or runtime postcondition checks;
unknown sizes in hypothetical concatenations must still be proved safe.
Arbitrary word equations remain outside this fragment.

The [slice example](slices.loom) proves byte lengths, empty/full ranges, nested
offsets and ordered adjacent-range reassembly through the same Text algebra.
`std.text.is_boundary` is a pure source helper for UTF-8 scalar boundaries.
Successful slice execution establishes its range and boundary facts; a slice
appearing only in a contract must prove both before normalization or SMT.
This includes empty ranges inside a multibyte scalar, which remain invalid.
Refinement weakening, `old`, compile-time calls and suspension reuse these rules;
the runtime slice still performs its ordinary bounds and UTF-8 checks.

The [composition example](composition.loom) combines correlated conditional
operands, Text lengths, integer arithmetic and pure helpers. It also selects an
entry List element by a conditional index and preserves that observation across
writes. The same composition rules handle each operation; proof-only helpers
stay absent from native code at both O0 and O2.

The [affine example](affine.loom) combines multiple equations over record fields,
Text lengths and entry List lengths through one bounded elimination rule. It
preserves the saved scalar length after growing a shared List, without claiming
that its current extent or contents stayed unchanged.

The [inequality example](inequalities.loom) derives a remaining budget from
multiple upper/lower bounds. One algebra handles record fields, callee summaries,
UTF-8 byte lengths and entry List lengths; a weighted numeric refinement also
weakens without a second construction check. Overflow and changed storage remain
separate obligations, and the proof-only budget predicate is absent from native IR.
