# Managed local lifetimes

Status: experimental, not selected for integration into main. Earlier reclamation
remains a goal; this particular analysis is not a completed memory-efficiency win.

Eight consecutive blocks build distinct `List[Int]` values and contribute their
last elements to a checked checksum. The locals are intentionally distinct:
putting the whole workload in a loop would reuse one slot and hide retention
until function exit. There is no manual collection or resource finalizer.

## Reproduce

Keep a baseline toolchain directory containing `loom`, `loom-native`, and
`libloom_runtime.a`, plus one fixed source snapshot containing `compiler/loom`
and `compiler/std`. Use the same snapshot for both compiler checks. After building
the candidate in the normal workspace:

```sh
node scripts/benchmark-gc.mjs BASELINE_TOOLCHAIN FIXED_SOURCES target/gc.json
node scripts/benchmark-gc.mjs BASELINE_TOOLCHAIN FIXED_SOURCES target/codegen.json --codegen
```

Set `LOOM_CC` to the LLVM 22 linker driver. The script is currently macOS-specific
because it parses `/usr/bin/time -l` and records its RSS units. It records source,
compiler, native-tool, runtime and executable hashes. Runtime comparisons use O3;
the self-hosted compiler and native codegen comparison use O2. Native codegen
consumes one fixed checked artifact, excluding source checking from that metric.

The default run measures ten alternating pairs after one warmup per executable,
at 1,048,576 and 8,388,608 elements per list, followed by a compiler source check.
The codegen mode measures three alternating pairs after warmup. Do not run other
local builds or tests concurrently. A small forced-GC run checks the checksum
before the timed runs; timed runs disable GC stress. Compare median and MAD,
not isolated samples or absolute timings from different sessions.

## Interpretation

Clearing a shadow root can make a graph collectible; a later collection may
actually free it. Neither fact alone establishes a reduction in process peak
RSS. Report managed live bytes, RSS, runtime and compiler cost separately.
The collector and host allocator can retain storage beyond the last source use.

Clears are generated only when later allocation is possible; function exit
already handles paths with no further collection. Conservative local clearing
is not complete lifetime precision: loop reads stay
rooted across backedges, assignments do not kill prior liveness, and temporary
expression snapshots retain their existing slot lifetimes. Ordinary scalar,
record and callback regressions are checked separately by the
[basic native comparison](../basic/README.md).

## Decision — 2026-09-08, Apple M4 Max

The first variant actually reclaims the preceding payload at the next collection:
read-only debugger samples show seven zero-byte managed-heap boundaries between
the eight payloads. Baseline retained bytes keep growing. These are post-GC
samples, not peak allocation measurements. The
[commands, binary hashes and samples](results/2026-09-08-reclamation.txt) preserve
that distinction; they do not establish the host allocator's release behavior.

The [ten-pair initial measurements](results/2026-09-08-initial-rss.json) did not
show an RSS improvement:

| Workload | Before peak RSS | Initial candidate peak RSS |
| --- | ---: | ---: |
| Eight 8 MiB lists | 68.20 MiB | 68.53 MiB |
| Eight 64 MiB lists | 516.20 MiB | 516.53 MiB |
| Compiler check | 218.65 MiB | 221.46 MiB |

Later variants remove redundant block/function clears and emit clears only when
future collection is possible. The final variant passes the four analysis tests
and five native cleanup/tuple/GC tests, including O0/O3 forced collection.
However, the [final three-pair fixed-LCIR comparison](results/2026-09-08-final-codegen.json)
still increases native codegen from **8,903.626 to 9,315.910 ms (+4.63%)**, with
MADs of 4.547/21.810 ms. This final variant has not completed a fresh bootstrap or
the full cross-language benchmark; do not conflate its validation with the first
variant's bootstrap. No runtime API or ownership syntax was added.

Keep this implementation and its evidence in the experiment branch, not as a
mandatory compiler layer. Revisit root representation and LLVM interaction with
a demonstrated end-to-end benefit; do not hold independent language/library work
behind this experiment.
