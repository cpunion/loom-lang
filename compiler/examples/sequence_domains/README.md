# Typed sequence domains

```sh
target/loom check compiler/examples/sequence_domains
target/loom test compiler/examples/sequence_domains
target/loom run compiler/examples/sequence_domains
```

Pure scans use the same checked element domains as individual reads: machine Int
bounds, byte values in 0..255, immutable Text lengths, and inline fields. These
facts are quantified only over valid indices of one logical storage version.
They neither justify out-of-bounds reads nor freeze storage across interference.

The example proves scan contracts without executing them at runtime. It also
updates a constrained sorted List's endpoints using the machine extrema. The
ordinary per-store proof establishes ordering after each write; aliases still
share the same storage. No sorting-specific update permission is added.
Both direct `values[length(values) - 1]` assignment and an extracted source
helper retain their index dependencies. Argument effects stay ordered and must
preserve the complete predicate themselves, including in shared builds.

`Sevens` combines an unexpanded pure size predicate with a quantified element
constraint. Its literal store is proved unchanged using the current cell's
quantified facts; it does not need to expand the size predicate or execute a
runtime preservation check.
