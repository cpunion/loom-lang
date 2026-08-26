# Benchmarking

The benchmark runner measures a small, controlled set of equivalent native
programs in Loom, Go, Rust, C, and C++. Its purpose is to detect performance
structure and regressions, not to rank languages.

## Prerequisites

Build the Loom compiler and runner in release mode. The default external tools
are `go`, Rust 1.88.0 through `rustup`, `clang`, and `clang++`:

```sh
cargo +1.88.0 build --locked --release -p loom-cli -p loom-benchmark
target/release/loom-benchmark --help
```

Override tools when necessary:

| Variable | Tool |
| --- | --- |
| `LOOM_BENCH_LOOMC` | Loom compiler binary |
| `LOOM_BENCH_GO` | Go command |
| `LOOM_BENCH_RUSTC` | Rust compiler command |
| `LOOM_BENCH_CC` | C compiler |
| `LOOM_BENCH_CXX` | C++ compiler |

Record every override with the report.

## Cases

| Case | Work |
| --- | --- |
| `int_lcg` | bounded integer arithmetic in a counted loop |
| `record_method` | mutable flat-record method calls |
| `list_build_scan` | grow and index-scan a list of integers |
| `fib_recursive` | non-tail recursive integer calls |

Each implementation receives the case, dynamic scale, and expected checksum.
The timed child must validate the checksum and print exactly `Unit`. A mismatch,
unexpected output, nonzero exit, or timeout fails the report.

## Profiles

| Profile | Defaults | Purpose |
| --- | --- | --- |
| standard | 3 warmups, 10 samples, 30 s timeout | repeatable local measurement |
| `--quick` | 1 warmup, 3 samples, 10 s timeout | correctness smoke, not performance evidence |
| `--throughput` | 2 warmups, 5 samples, 60 s timeout | amplified hot-path measurement |

Standard scales are 2,000,000 (`int_lcg`), 500,000 (`record_method`), 10,000
(`list_build_scan`), and 32 (`fib_recursive`). Throughput scales are
100,000,000, 100,000,000, 10,000,000, and 40 respectively.

Override the sample plan only for an explicit experiment:

```sh
target/release/loom-benchmark \
  --throughput \
  --case int_lcg \
  --case record_method \
  --warmups 3 \
  --runs 10 \
  --timeout-seconds 90 \
  --output target/benchmark.json
```

## Measurement policy

The runner builds each language once before measuring and excludes compilation
from runtime samples. It rotates language execution order by case and round.
The timed interval starts immediately before spawning the direct child and ends
when the operating system reports that child has exited. It includes process
startup, argument parsing, benchmark work, and child-side checksum validation.
It excludes post-exit stdout/stderr pipe draining and parent-side output
validation. Reports identify this policy as
`spawn_to_exit_notification`.

For standard and throughput profiles on Linux/macOS, the runner rejects a
1-minute load average above the greater of 1.0 and 75% of logical CPU count.
`--allow-busy-host` records that the guard was overridden; such a report is
explicitly noisy. Quick mode skips the load guard.

Run on an idle, fixed-power machine. Do not combine reports from different
CPU, OS, toolchain, profile, scale, or compile flags into one relative ranking.

## Report

The schema-1 JSON report records:

- OS, architecture, CPU, logical CPUs, and pre-build load;
- exact compiler versions and compile argument vectors;
- compile time, binary size, and source SHA-256;
- profile, timeouts, warmups, runs, timing policy, and busy-host override;
- expected checksum and raw nanosecond samples;
- minimum, p05, median, mean, p95, maximum, and relative median.

Preserve the raw report. With only five throughput samples, p05 and p95 reduce
to the extremes and are not stable tail-latency estimates.

## Compile policy

The runner currently uses:

- Loom `--release --no-cache`;
- Go `build -trimpath -ldflags="-s -w"` with `GOMAXPROCS=1` at runtime;
- Rust edition 2024, `opt-level=2`, overflow checks, one codegen unit;
- C17 and C++20 with `-O2 -DNDEBUG` and warnings denied.

There is no cross-language LTO assumption. When changing these flags, change
the report schema/policy text and explain why the comparison remains coherent.

## PR benchmark workflow

The PR workflow measures base and candidate merge revisions on Ubuntu 24.04
x86-64 and macOS 15 arm64 runners with pinned Rust, LLVM/Clang, and Go. Each
runner uploads one base/candidate report pair. A separate `workflow_run` job
checks out the trusted default branch renderer, validates both artifacts, and
updates one sticky PR comment.

The comment contains one exact `base | candidate | delta` table across both
measured platforms, followed by separate macOS and Linux runtime-index charts.
The charts are stacked so every category remains readable in GitHub comments.
Each chart contains only its own platform: macOS uses blue candidate bars,
Linux uses orange candidate bars, and a gray line marks that platform's base
revision at 100. The panels use the same numeric scale, but every index is
normalized only against its same-platform base. Lower values are faster and
higher values are slower. The table remains the exact and accessible source of
truth. Windows benchmark evidence remains unavailable; its complete native CI
job must first establish compiler, runtime, and I/O execution on a real runner.

The comment is informational. Each shared runner passes
`--allow-busy-host`, so it is useful for spotting large changes, not for
publishing stable language ratios.

## Adding or changing a case

Update all five source fixtures, the Rust case table and checksum oracle,
profile scales, help/README text, and report tests together. Verify:

1. identical bounded work and overflow behavior;
2. no constant precomputed checksum in a timed implementation;
3. dynamic checksum failure is observable;
4. all implementations print the same output;
5. quick mode completes comfortably;
6. profiles have enough duration without approaching their timeout;
7. any claimed cause is supported by IR, assembly, allocation, or profiler
   evidence rather than wall time alone.
