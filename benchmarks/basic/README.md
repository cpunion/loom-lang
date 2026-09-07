# Native basic benchmarks

Five equivalent programs exercise signed 64-bit arithmetic, recursive calls,
scalar records returned by value, growable collections, and function values.
This measures native executables, not an interpreter or compiler throughput.

## Run

Build Loom first, then select the intended C, Go, Rust and Zig toolchains:

```sh
BENCH_CC=clang BENCH_RUSTC=rustc node scripts/benchmark-basic.mjs --runs 9
node scripts/benchmark-basic.mjs --reuse-build --runs 9
```

`BENCH_LOOM`, `BENCH_GO`, `BENCH_ZIG` and `LOOM_CC` can also select tools.
`--quick --runs 1` validates the harness with small inputs, not useful timings.
Builds and raw reports stay under `target/performance/`. Reuse checks binary
hashes against the original build manifest and retains that provenance.

Each executable receives `CASE N SEED` at runtime and prints its checksum.
An independent reference checks every output. One warmup precedes nine fresh
process samples; variant order rotates each round. Measurements include process
startup, argument parsing, allocation, output and exit, without subtracting a
baseline. No `volatile`, forced `noinline`, optimizer barriers, or identical-call
repeat loop prevents legitimate optimization. Function values may devirtualize;
this is not a forced indirect-call latency test.

| Case | Work, with seed 17 |
| --- | --- |
| `int_lcg` | 20 million bounded LCG updates |
| `fib_recursive` | Naive recursive fib(38) |
| `record_value` | 15 million three-field value updates |
| `list_build_scan` | Grow from empty to 12 million Ints, then sum once |
| `function_value` | 20 million calls to a runtime-selected function value |

Collections do not reserve capacity upfront. C/Loom double from a minimum of
eight; Go, Rust and Zig retain native growth policies. C/Rust/Zig explicitly
release storage, while Loom/Go use GC. The collection case includes those library
and allocator differences, not just scalar syntax. Loom caches the scan length.
Zig links libc in both modes so `init.gpa` consistently selects `c_allocator`,
not different debug and release allocators.

## macOS result: 2026-09-07

Apple M4 Max, 128 GiB RAM, macOS/Darwin 25.2.0, arm64. Native Loom uses LLVM
22.1.8 and the Rust runtime at opt-level 3 with debug checks retained; C uses
Clang 22.1.8, Go 1.27.0, Rust 1.93.0
(LLVM 21.1.8), and Zig 0.16.0. C/Rust/Zig select the host CPU; Go uses its default
`GOARM64=v8.0`, GC and optimized build. This is one host, not a cross-platform
language ranking. [Raw samples, hashes, flags and build observations](results/2026-09-07-macos-arm64.json)
retain the exact measurement basis, including the dirty-tree/source hashes.

Milliseconds, median of nine; smaller is faster:

| Case | Loom O3 | C O3 | Go | Rust O3 | Zig ReleaseFast |
| --- | ---: | ---: | ---: | ---: | ---: |
| Integer LCG | 71.647 | 71.991 | 76.740 | 65.824 | 71.370 |
| Recursive fib | 91.436 | 67.431 | 90.037 | 68.357 | 71.160 |
| Record value | 57.879 | 56.334 | 50.193 | 56.732 | 67.117 |
| List build + scan | 231.800 | 52.938 | 77.686 | 52.138 | 56.358 |
| Function value | 70.880 | 65.590 | 85.482 | 66.246 | 86.579 |

Loom retains overflow checks. C O3, Rust with overflow checks off, and Zig
ReleaseFast do not provide the same integer-checking behavior; Go retains its
ordinary wrapping integer semantics. These are normal optimized modes, not
identical safety guarantees. The integer-checking variants below are separate;
C `-ftrapv` still does not add collection bounds checking.

| Case | Loom O3 | C O3 + ftrapv | Rust O3 + overflow checks | Zig ReleaseSafe |
| --- | ---: | ---: | ---: | ---: |
| Integer LCG | 71.647 | 69.652 | 66.496 | 70.965 |
| Recursive fib | 91.436 | 92.187 | 91.432 | 89.150 |
| Record value | 57.879 | 55.973 | 56.409 | 64.903 |
| List build + scan | 231.800 | 54.262 | 52.293 | 58.298 |
| Function value | 70.880 | 67.831 | 68.354 | 88.285 |

The startup control is about 1.6–2.4 ms. Workload median absolute deviations are
about 0.2–3.8 ms; the raw report includes every sample. Loom's default O2 results
are close to O3, with no consistent O3 advantage here. One-off build invocations
are recorded separately, but differing warm tool caches make them unsuitable
for ranking compiler throughput.

## Interpretation

Integer LCG is approximately tied with C, while records and function values
are about 3% and 8% slower. Fib's apparent 36% gap against unchecked C disappears
against the integer-checking C/Rust variants. None establishes whole-language
performance equivalence.

The baseline gap is List build/scan: about 4.4x C/Rust and 3.0x Go. Inspection of
that baseline's optimized Loom IR identified these costs:

- Each push retains two nested GC root entry/exit pairs after the source
  wrappers inline, despite the enclosing function already having a root frame.
- Each scan element crosses an opaque `list_get` runtime boundary, checks its
  index, computes a dynamic-stride offset and performs a checked scalar addition.
- Loom growth allocates zeroed storage and copies initialized elements;
  C `realloc` can extend in place. Old Loom buffers are reclaimed by GC later.

This is not an executor per element, collection on every push, or tracing of
individual Int elements. The LCG checks in the build loop are already eliminated.
IR inspection identifies costs but does not assign measured percentages to
them. No compiler/runtime optimization was made during this baseline run.
To inspect the current compiler's IR (not necessarily the archived baseline):

```sh
LOOM_OPT_LEVEL=3 target/loom build benchmarks/basic \
  --output target/basic-inspect --emit-ir target/basic-inspect.ll
```
