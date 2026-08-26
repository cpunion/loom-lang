# Basic cross-language benchmarks

This suite compares release-mode Loom/LLVM programs with equivalent Go, Rust,
C, and C++ programs on the same host. It is a compiler-development instrument,
not a general language ranking.

Each executable accepts `CASE SIZE EXPECTED`, computes the selected workload
from runtime inputs, validates its checksum, and prints `Unit` on success. The
expected value is supplied at runtime so an optimizer cannot replace the hot
calculation with a source constant.

## Workloads

| Case | Standard scale | Primary path | Checksum |
| --- | ---: | --- | --- |
| `int_lcg` | 2,000,000 | Bounded integer arithmetic and a loop | Final generator state |
| `record_method` | 500,000 | Record values, a mutating receiver, and cross-function calls | Total and call count |
| `list_build_scan` | 10,000 | Geometric list growth followed by a complete scan | Element sum and length |
| `fib_recursive` | 32 | Non-tail recursive calls | Fibonacci value |

All configured values fit in a signed 64-bit integer. The list workload begins
with an empty collection in every language; it measures allocation growth,
writes, and bounds-safe reads together. Text concatenation and dynamic dispatch
are intentionally absent because equivalent allocation and devirtualization
semantics have not yet been specified across all five implementations.

The current Loom benchmark executable is intentionally one runtime-selected
program: its root parses `List[Text]`, matches the requested case, and keeps all
workloads reachable. Automatic native routing is whole-artifact atomic, so the
remaining Text/List/parse/match coverage selects the legacy native route for
this executable, including `record_method`. Do not cite this suite as direct
LCIR record performance yet. Separate source/interpreter/LLVM differential
tests build and run a closed record-method workload through direct LCIR; moving
the parameter-driven benchmark itself requires a future per-case build protocol.

## Run the suite

Build the Loom compiler and benchmark runner first:

```sh
cargo +1.88.0 build --release --locked -p loom-cli -p loom-benchmark
target/release/loom-benchmark --output target/basic-benchmark.json
```

Use the smaller correctness profile while changing a workload or the runner:

```sh
target/release/loom-benchmark \
  --quick \
  --output target/basic-benchmark-quick.json
```

Use the amplified profile when process startup dominates the standard scale:

```sh
target/release/loom-benchmark \
  --throughput \
  --output target/basic-benchmark-throughput.json
```

`--quick` and `--throughput` are mutually exclusive. Repeat `--case NAME` to
select workloads, or override sampling with `--warmups N`, `--runs N`, and
`--timeout-seconds N`. Standard and throughput runs reject a busy host by
default; `--allow-busy-host` records an explicitly noisy diagnostic run.

The runner normally invokes `target/release/loomc`, `go`, Rust 1.88.0, `clang`,
and `clang++`. Override individual tools with `LOOM_BENCH_LOOMC`,
`LOOM_BENCH_GO`, `LOOM_BENCH_RUSTC`, `LOOM_BENCH_CC`, and
`LOOM_BENCH_CXX`.

## Reading a report

The JSON report records source digests, tool versions, compile commands, host
load, deadlines, one build-time sample, binary size, raw runtime samples, and
summary statistics. Runtime covers a complete child process from spawn to exit,
including argument parsing and checksum validation.

Interpret results conservatively:

- compare base and candidate revisions on the same runner;
- do not average ratios across hosts or operating systems;
- treat shared-runner wall time as diagnostic evidence, not a release promise;
- do not infer tail latency from the five-sample throughput profile;
- remember that build time is a single cold-like sample and binary size depends
  on linking and stripping policy; and
- inspect generated LLVM IR, assembly, or hardware counters before attributing a
  difference to one optimization.

The pull-request workflow follows these rules and posts a base-versus-candidate
summary. See the [benchmarking guide](../../docs/contributing/benchmarking.md)
for the evidence policy and reproduction checklist.
