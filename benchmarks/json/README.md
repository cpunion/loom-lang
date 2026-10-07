# Incremental JSON benchmark

From the repository root:

```sh
target/loom build benchmarks/json --output target/json-benchmark
target/json-benchmark
```

Each line reports parse time in nanoseconds. Inputs and chunks are prepared
outside the timed region; every result is checked. Cases cover a 10,001-element
number array in 4 KiB chunks (30 parses), a 256 KiB string in 4 KiB chunks
(30 parses), and the same array in one-byte chunks (3 parses).

Compare native binaries built from the same source at the same optimization
level. Alternate baseline/candidate runs and report medians; do not mix compiler
build time with decoding time. This measures the incremental byte API, not
contiguous `parse(Text)` or network scheduling.
