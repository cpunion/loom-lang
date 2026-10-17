# Append with shared handles

```sh
target/loom check compiler/examples/list_append
target/loom test compiler/examples/list_append
target/loom run compiler/examples/list_append
```

`append` proves its length change, retained target prefix, and appended entry
source at the original target length. The inputs may alias, including self-append.
Each checked push preserves existing elements, but another handle's new length
or tail value is not assumed. Destructive writes and shared interference still
invalidate unsupported observations. Proofs add no runtime snapshots or checks.

`columns.loom` frames inline Bool/Text/Float fields with the same rule. Its Float
equality contract explicitly excludes NaN; preserving a representation does not
make IEEE NaN equal to itself.

`histograms.loom` composes that frame with finite occurrence counts: when the
other handle's length stays unchanged, its whole histogram stays unchanged.
No equality is inferred for a new tail or for a count extending past the frame.
