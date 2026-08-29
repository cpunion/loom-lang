# Quality policy

Loom accepts claims in proportion to reproducible evidence. A design document
can explain intended behavior; only executable tests establish current
implementation status.

## Evidence levels

Use the narrowest accurate label:

| Evidence | Supports |
| --- | --- |
| Design or RFC | A proposed decision, not implementation. |
| Unit test | One component or invariant under controlled inputs. |
| Integration test | A boundary between components, artifacts, or processes. |
| Differential test | Equivalent observable behavior across interpreter and native backends. |
| End-to-end fixture | A real source project completing check/build/test/run. |
| Platform CI | The exact commands on the named runner and architecture. |
| Release gate | The exact binaries and runtime bundle archived by the release workflow. |
| External production evidence | Only the independently operated workload that generated it. |

Passing on one platform does not imply support on another. Object emission does
not imply executable runtime support. Building a crate does not imply the CLI
or standard library works on that host.

## Required change evidence

Language or semantic changes need:

- accepted and rejected source examples;
- stable diagnostic assertions where user behavior changes;
- MIR validation coverage;
- interpreter/native differential coverage when both backends apply;
- artifact and cache invalidation coverage for changed wire facts;
- documentation updated in the same pull request.

Runtime or ABI changes additionally need malformed-boundary tests, forced GC or
Task-state tests as applicable, and runtime-bundle compatibility tests.

Optimization changes need a checked fallback, semantic differential tests, and
IR/object structure tests. Performance measurements do not permit disabling
contracts, overflow checks, cleanup, GC roots, or concept proof validation.

## Controlled quality runner

`loom-quality` runs the frozen Core fixtures, a required typed-LCIR fixture, the
C3 multi-package project, standard-library behavior, parser throughput,
artifact decoding, and a 64-module incremental edit under generous upper
bounds. It emits a versioned JSON evidence report.

Every native object is prepared and emitted through the production prepared
route. Schema 3 records the scenario, expected LCIR route, actual route, and
whether they agree. Every controlled fixture must select LCIR; any other route
fails the runner. The evidence schema has no exception or allowance field.

Run and test artifacts are judged independently because their exact reachable
graphs may differ. The constraints-and-contracts fixture requires typed LCIR
for both graphs; source contracts and nongeneric runtime-checked constrained
construction lower directly.

Those time bounds detect gross regressions and runaway behavior on CI; they are
not user latency service-level objectives. The C3 label in the report means
“controlled multi-package repository evidence,” not maturity level 3 or
external production validation.

## Benchmarks

`loom-benchmark` compares equivalent Loom, Go, Rust, C, and C++ microfixtures.
It validates dynamic checksums, builds once, records raw nanosecond samples and
toolchain/host identity, and rotates execution order.

Every report states: “Controlled microbenchmark evidence, not a general
language ranking.” Preserve that warning in derived reports.

Do not:

- compare reports from different machines/toolchains as a language ranking;
- interpret quick smoke measurements as steady-state throughput;
- infer allocation, GC, or instruction causes from wall time alone;
- hide `--allow-busy-host` when it was used;
- publish only a relative multiplier without raw reports and commit identity.

See [Benchmarking](../contributing/benchmarking.md).

## Fuzzing and hostile inputs

Fuzz targets cover lossless syntax/recovery, artifact decoding plus MIR
validation, and constrained-integer proof/runtime semantics. Registry bundles,
cache entries, runtime bundles, descriptors, and CLI JSON boundaries also need
deterministic hostile-input tests.

A fuzzer crash is fixed only after minimization and promotion to a stable
regression test. A saved artifact without a deterministic regression is not a
complete fix.

## Flaky and skipped tests

Do not solve environmental flakiness by weakening semantic assertions.
Platform-specific tests must state the prerequisite and fail clearly or be
selected by an explicit platform condition. New ignored tests require a linked
issue and a reason; permanently ignored evidence must not be listed as
implemented status.
