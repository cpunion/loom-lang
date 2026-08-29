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

Every change needs the smallest deterministic test that would have caught its
regression. Add another layer only when the change crosses that boundary. For
example, a diagnostic change needs a source-level diagnostic assertion, while
an artifact schema change needs decoding and cache-identity coverage. Do not
duplicate the same invariant across unit, integration, differential, and
end-to-end suites by default.

Language behavior shared by both backends needs one representative
interpreter/native differential test. Runtime, ABI, and hostile-input changes
need boundary tests proportional to the risk they introduce. Optimization
changes need semantic evidence and a checked fallback; use an IR or object
assertion only when machine structure is the claim being made.

User-visible behavior and its documentation change in the same pull request.
Performance measurements never permit disabling contracts, overflow checks,
cleanup, GC roots, or concept proof validation.

## Development gate

The default pull-request and `main` gate runs on macOS 15 arm64 with LLVM 19.
It formats and lints the workspace, prepares one runtime sidecar, runs the Rust
workspace tests once, executes the Loom standard-library tests on the native
and interpreter backends, and checks repository documentation. It deliberately
does not repeat separate Cargo `check`, `test`, and complete `build` passes.

Linux and Windows are release-matrix platforms, not per-change development
gates. A passing development gate is therefore macOS evidence only. Base versus
candidate benchmarks are opt-in through the `benchmark` pull-request label,
and fuzz campaigns are run explicitly when the affected trust boundary needs
them.

## Controlled quality runner

`loom-quality` runs the frozen Core fixtures, a required typed-LCIR fixture, the
C3 multi-module project, standard-library behavior, parser throughput,
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
