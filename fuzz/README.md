# Fuzzing Loom

The fuzz workspace is separate from the compiler workspace so stable builds do
not acquire a libFuzzer dependency.

It contains three targets:

- `syntax` checks lossless UTF-8 lexing, monotonic tokens and spans, bounded
  parser recovery, and valid diagnostic spans;
- `artifact` applies raw and structured mutations to a valid artifact, then
  exercises envelope decoding, floating-point restoration, entry validation,
  and the complete checked-MIR validator; and
- `semantics` generates bounded constrained-integer programs and compares proof
  elimination, rejected literals, runtime validation, and interpreter results.

Use the same pinned nightly and `cargo-fuzz` release as CI:

```sh
cargo install cargo-fuzz --locked --version 0.13.1
cargo +nightly-2025-06-26 fuzz run syntax -- -max_total_time=60 -timeout=5
cargo +nightly-2025-06-26 fuzz run artifact -- -max_total_time=60 -timeout=5
cargo +nightly-2025-06-26 fuzz run semantics -- -max_total_time=60 -timeout=5
```

libFuzzer writes findings below `fuzz/artifacts/`. Minimize every reproducer and
promote it to an ordinary deterministic regression test before considering the
finding resolved. CI runs all three targets for a bounded smoke interval; longer
campaigns remain a maintainer task.

See the [fuzzing guide](../docs/contributing/fuzzing.md) for triage, corpus, and
reproduction policy.
