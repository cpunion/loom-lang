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

To compare an archived compiler result in the same session, keep its `loom-o3`
executable and `build.json` together and add `--baseline directory`. The runner
verifies the binary and matching Loom source hash before adding that variant.

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

## Managed-memory lowering repair

Measured on the same host on 2026-09-07, after commit `17c4245`. Ten rotated
rounds include the archived baseline Loom O3 executable alongside all nine
current variants: each occupies every position once. Inputs and source files
are unchanged. [Raw samples and both build manifests](results/2026-09-07-macos-arm64-memory.json)
retain hashes, flags and tool versions. Host timing changed since the first run;
compare the before/after columns here, not absolute times across sessions.

Milliseconds, median of ten:

| Case | Loom before | Loom O3 now | Change | C O3 | Go | Rust O3 | Zig ReleaseFast |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Integer LCG | 101.626 | 101.579 | -0.0% | 101.031 | 109.080 | 94.530 | 101.584 |
| Recursive fib | 123.017 | 125.913 | +2.4% | 93.100 | 123.096 | 93.416 | 97.921 |
| Record value | 80.491 | 74.105 | -7.9% | 77.862 | 68.854 | 77.785 | 91.824 |
| List build + scan | 308.409 | 77.648 | -74.8% | 71.665 | 108.121 | 68.414 | 75.787 |
| Function value | 98.597 | 99.021 | +0.4% | 94.501 | 120.513 | 94.705 | 123.630 |

Loom still checks arithmetic and bounds. With integer checks enabled, C/Rust
fib take 125.319/127.098 ms; their List results are 75.033/71.679 ms.
Loom's List time is now about 8% above unchecked C and 13% above unchecked Rust,
not 4.4x. The small fib regression remains visible; this is not an improvement
in every kernel or a whole-language performance guarantee.

The repair is general: finalize linked stack roots after source inlining, retain
only allocation-crossing temporary snapshots, and keep ordinary locals separate
from their GC shadows. Typed List/Text/Bytes accesses no longer cross an opaque
runtime accessor. Push uses typed stores and calls the runtime only for capacity
growth; private backing storage can reallocate in place. Obsolete accessor/push
ABI symbols were removed. Safety checks, shared headers and tracing of initialized
managed elements remain; this does not implement moving GC or fault unwinding.

A separate ten-pair alternating test checks the same `compiler/loom` and `std`
sources with the before/after O2 compiler executables. Whole-check median fell
from 489.622 to 245.303 ms (-49.9%); median peak RSS fell from 91.0 to 89.1 MiB.
Median absolute deviations were 2.786/1.636 ms.
[Compiler samples and source/binary hashes](results/2026-09-07-macos-arm64-compiler-memory.json)
cover this comparison. It measures the native frontend, not LLVM build throughput
or incremental reuse. Full Rust 1.88 validation and byte-identical bootstrap
stages 2/3 also pass locally; cross-platform CI remains a separate gate.

## Moving collector

Commit `2b2c04d` adds relocating roots, small-object copying arenas and traced
large objects. Ten rotated rounds compare it with the archived nonmoving
compiler/runtime, on the same host and unchanged basic inputs.
[Raw results](results/2026-09-07-macos-arm64-moving-basic.json) retain both
build manifests and all samples. This session had substantial host variation;
small deltas below the reported MAD do not establish a regression or improvement.

Milliseconds, median of ten:

| Case | Nonmoving Loom O3 | Moving Loom O3 | C O3 | Go | Rust O3 | Zig ReleaseFast |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Integer LCG | 77.856 | 77.210 | 76.475 | 84.214 | 72.376 | 79.170 |
| Recursive fib | 149.899 | 145.357 | 111.864 | 151.943 | 108.472 | 118.667 |
| Record value | 84.086 | 85.960 | 89.927 | 79.475 | 89.790 | 101.275 |
| List build + scan | 81.069 | 83.616 | 76.656 | 122.733 | 70.862 | 78.620 |
| Function value | 146.091 | 139.225 | 131.026 | 176.245 | 122.497 | 162.966 |

List is +3.1%, with before/after MADs of 4.621/4.852 ms. The other nontrivial
kernels vary from -4.7% to +2.2%; fib, record and callback samples are especially
noisy. The arithmetic-safety differences above still apply: checked C/Rust fib
medians are 143.967/159.615 ms. This is not a whole-language performance ranking.

A separate ten-pair alternating check uses the same archived `compiler/loom`
and `std` sources at `778cfe1`, warm OS caches and no incremental cache.
[Samples and source/compiler hashes](results/2026-09-07-macos-arm64-moving-check.json)
record CPU, wall time and peak RSS. Median CPU time decreases from 950 to 695 ms
(-26.8%), with MADs of 60/25 ms; macOS rounds these CPU counters. Wall medians
are 1464.943/1036.224 ms, but their 402.433/137.204 ms MADs show host contention.
Peak RSS increases from 149.2 to 197.4 MiB (+32.3%). Copying collection needs
temporary old/new storage; this memory cost remains visible, not an achieved
memory-efficiency target. Small allocations are batched, obsolete arena slices
remain charged until collection, and private pointer-map entries avoid redundant
metadata. Ordinary access still needs no read barrier or executor.

## Main-based refresh — 2026-09-08

The source-only UTF-8 change at `49ad5e8` uses the main backend, not the local-root
experiment. Ten rotated rounds on the same M4 Max compare it with the archived
compiler at `fbf9f0f`. The [complete report](results/2026-09-08-macos-arm64-basic.json)
records all samples and toolchain hashes. C uses LLVM 22.1.8, Go 1.27.0,
Rust 1.95.0-nightly (`eda76d9d1`, LLVM 21.1.8), and Zig 0.16.0. The Loom backend
itself is built with Rust 1.88; that is separate from the comparison's Rust tool.

Build-manifest revisions describe the benchmark driver's checkout, not necessarily
the selected compiler. The baseline manifest records a dirty `356de6e1` checkout,
but explicitly uses the pre-experiment archived compiler (`98821abe...`) and
native tool (`804621a6...`); their full hashes identify the measured tools.
The candidate's dirty flag reflects a roadmap-only edit at measurement time.
The raw metadata is retained unchanged, rather than presenting clean-checkout runs.

Milliseconds, median of ten:

| Case | Loom O3 | C O3 | Go | Rust O3 | Zig ReleaseFast |
| --- | ---: | ---: | ---: | ---: | ---: |
| Integer LCG | 74.395 | 73.633 | 79.154 | 68.379 | 74.166 |
| Recursive fib | 90.002 | 68.777 | 89.860 | 67.419 | 71.616 |
| Record value | 54.185 | 56.992 | 50.273 | 56.790 | 66.926 |
| List build + scan | 58.830 | 53.813 | 79.063 | 51.328 | 57.000 |
| Function value | 72.050 | 68.634 | 87.809 | 69.207 | 90.165 |

Loom's five kernel deltas against its same-session baseline range from -0.65%
to +0.60%; this library change is not a runtime optimization. Do not interpret
lower absolute times than the previous day's session as a compiler speedup.
Safety still differs: checked C/Rust fib are 90.539/92.235 ms, near Loom's
90.002 ms. The unchecked ranking is not a whole-language performance conclusion.

A [fixed-source ten-pair check](results/2026-09-08-macos-arm64-check.json) measures
the same 145 source files: median CPU is 340/340 ms, wall 351.682/356.978 ms,
and peak RSS 219.61/229.37 MiB. These are fresh source checks with warm OS caches,
not LLVM builds or incremental reuse. No frontend-memory improvement is claimed.

The separate [local-root experiment](https://github.com/cpunion/loom-lang/pull/240)
was not integrated: it demonstrably reclaims dead graphs, but its initial RSS
measurements did not improve and its final fixed-LCIR codegen cost remained
+4.63%. Its code, test evidence and measurements remain on the experiment branch;
the early-reclamation goal is still open without adding that layer to main.
