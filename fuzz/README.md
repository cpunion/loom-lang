# Loom fuzzing

The fuzz workspace is intentionally separate from the compiler workspace so
normal stable builds do not acquire a libFuzzer dependency.

Targets:

- `syntax` checks lossless UTF-8 lexing, token/span monotonicity, bounded parser
  recovery, and diagnostic span validity.
- `artifact` feeds both raw bytes and structured mutations of a valid seed
  through envelope decoding, Float restoration, entry checking, and the full
  checked-MIR validator.

Run locally with a nightly toolchain and `cargo-fuzz` 0.13.1:

```sh
cargo install cargo-fuzz --locked --version 0.13.1
cargo +nightly fuzz run syntax -- -max_total_time=60 -timeout=5
cargo +nightly fuzz run artifact -- -max_total_time=60 -timeout=5
```

Crashes are written below `fuzz/artifacts/` and must be minimized and promoted
to an ordinary deterministic regression test before the finding is considered
closed. CI runs both targets for a bounded smoke interval on every change.
