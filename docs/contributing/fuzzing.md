# Fuzzing

The `fuzz/` directory is a separate Cargo workspace so ordinary stable builds
do not depend on libFuzzer. It uses a pinned nightly and `cargo-fuzz` release.

## Targets

| Target | Invariants |
| --- | --- |
| `syntax` | UTF-8 lexer losslessness, monotonic tokens/spans, bounded parser recovery, valid diagnostic spans |
| `artifact` | arbitrary bytes and structured mutations through envelope decoding, float restoration, entry checking, and complete MIR validation |
| `semantics` | bounded constrained-integer programs across proof elimination, source rejection, runtime validation, and interpreter results |

## Reproduce the CI configuration

```sh
rustup toolchain install nightly-2025-06-26
cargo install cargo-fuzz --locked --version 0.13.1

cargo +nightly-2025-06-26 fuzz run syntax -- \
  -max_total_time=20 -timeout=5 -rss_limit_mb=2048
cargo +nightly-2025-06-26 fuzz run artifact -- \
  -max_total_time=20 -timeout=5 -rss_limit_mb=2048
cargo +nightly-2025-06-26 fuzz run semantics -- \
  -max_total_time=20 -timeout=5 -rss_limit_mb=2048
```

For a local campaign, increase `-max_total_time`. Keep the per-input timeout
and a reasonable RSS bound so a single pathological input does not stall the
campaign.

## Reproducing a finding

libFuzzer writes findings under `fuzz/artifacts/TARGET/`. Reproduce with:

```sh
cargo +nightly-2025-06-26 fuzz run TARGET \
  fuzz/artifacts/TARGET/FILE
```

Then minimize it:

```sh
cargo +nightly-2025-06-26 fuzz tmin TARGET \
  fuzz/artifacts/TARGET/FILE
```

If structured artifact mutation obscures the failure, preserve both the raw
input and a human-readable decoded explanation during triage. Do not edit the
input before confirming the original reproduces.

## Fix policy

A complete fix:

1. identifies whether the bug is panic, timeout, unbounded allocation,
   validator acceptance, or semantic mismatch;
2. minimizes the input;
3. adds an ordinary deterministic regression test at the narrowest layer;
4. fixes the underlying boundary without weakening the fuzzer invariant;
5. reruns the target and relevant stable workspace tests.

Do not commit large generated corpora or crash dumps. Commit a small minimized
fixture only when constructing the regression in Rust would lose the essential
wire shape.

## Extending fuzz coverage

Add a new target only when it has a clear invariant and bounded execution.
Prefer extending an existing target's generator/mutator when the new input
shares the same trust boundary. Seed corpora must contain no secrets,
machine-specific paths, or copyrighted third-party corpus dumps.

Fuzzing supplements deterministic hostile-input tests. Runtime bundles,
registries, filesystem races, OS readiness, and linker process behavior often
need explicit integration tests rather than in-process libFuzzer targets.
