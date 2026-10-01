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

The [byte-length example](lengths.loom) proves UTF-8 byte counts (six for `é😀`,
not two characters), preserved entry lengths and equal-value lengths. Checked
1–8-byte names widen to a 1–16-byte type without a second check, while unknown
construction still returns a checked Result. Length constraints remain valid
across GC and suspension. Equal lengths do not imply equal contents;
concatenation and substring properties remain outside this proof fragment.
