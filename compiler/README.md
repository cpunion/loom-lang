# Native compiler

The [Loom-written compiler](loom/README.md) implements package loading, parsing,
binding, type checking, bounded required proofs, and checked program emission.
It builds further compiler stages using one retained Rust LLVM/platform tool.
The [roadmap](../ROADMAP.md) distinguishes the native bootstrap from completion
of the accepted language. A previous Loom compiler is the bootstrap input;
a fresh checkout produces that input from the source-bound portable checked seed.

The native tool consumes a checked program, not source that it parses or
type-checks again. It uses LLVM 22 through Inkwell; there is no second language
frontend or runtime interpreter. Cleanup-free scalar programs link only
the host C library for fault reporting; managed programs also link the small
Rust runtime. Ordinary arithmetic and calls lower
directly; LLVM's O2 pipeline promotes local storage and removes unused code.

The [codegen boundary](src/codegen.rs) consumes the checked program and emits a
native object plus linking requirements. Its optimization levels and options do
not expose Inkwell types. LLVM is the only implementation today; target-machine
setup, passes, and tuning stay inside that backend. Another backend can reuse
the source frontend, proofs, checked input, host linker, and runtime boundary
without an alternate language implementation or a dispatch/plugin framework.

## Build and try it

Use Rust 1.88 (pinned by `rust-toolchain.toml`), LLVM 22 development libraries,
Clang, and Z3 on `PATH` for extended contract proofs. CI uses
[Z3 5.1.0](https://github.com/Z3Prover/z3/releases/tag/z3-5.1.0).
The solver runs only at compile time when the fast prover cannot finish;
ordinary checking and emitted applications do not require it. macOS, Linux, and Windows
pass the LLVM 22 bootstrap and native gate. On Ubuntu 24.04 use the signed
[LLVM apt repository](https://apt.llvm.org/) and install `llvm-22-dev`, `clang-22`,
and `libpolly-22-dev`; set `LLVM_SYS_221_PREFIX=/usr/lib/llvm-22` and
`LOOM_CC=/usr/bin/clang-22`. The [CI recipe](../.github/workflows/ci.yml) shows
repository setup and runs the same full native/bootstrap gate.
From the repository root on macOS:

```sh
brew install llvm@22 z3
export LLVM_SYS_221_PREFIX="$(brew --prefix llvm@22)"
export LOOM_CC="$LLVM_SYS_221_PREFIX/bin/clang"
bash scripts/bootstrap.sh

target/loom check compiler/examples/scalar
target/loom build compiler/examples/scalar \
  --output target/scalar --emit-ir target/scalar.ll
target/scalar
target/loom test compiler/examples/scalar
target/loom run compiler/examples/data
target/loom test compiler/std/loom/checking
target/loom test compiler/std/result
target/loom test compiler/std/list
LOOM_GC_STRESS=1 compiler/std/list/target/tests
target/loom test compiler/std/list --no-run --output target/list-tests
target/loom build compiler/examples/arguments --output target/arguments
target/arguments +0010

target/loom init hello
target/loom test hello
target/loom run hello
```

The [bootstrap script](../scripts/bootstrap.sh) builds the current Rust tool
and runtime, then Loom stages 1, 2, and 3. It compares stages 2/3 byte-for-byte
and publishes `target/loom`. On every host, a cold build verifies and decompresses
`compiler/bootstrap/stage0.checked.gz`, then compiles that portable checked
program with the current native bridge. This source-bound stage 0 builds the
current Loom source directly: no historical Rust frontend or checkpoint chain
is built. A fresh or shallow checkout needs no installed Loom compiler and no
historical commits for bootstrapping. LLVM 22 serves every stage.

The seed is checked IR, not a host-specific machine executable. Unix CI
reproduces its bytes from the immutable source pin; that
verification requires the pinned commit's Git history. See the seed maintenance
commands in [bootstrap seed maintenance](#bootstrap-seed-maintenance).
The seed is a trusted bootstrap input: self-hosting agreement and source
reproduction are consistency checks, not compiler correctness proofs.

An existing compatible Loom compiler bypasses checked seed compilation:

```sh
LOOM_BOOTSTRAP_COMPILER=/path/to/loom bash scripts/bootstrap.sh
```

For normal compiler edits, rebuild once with the installed `target/loom`:

```sh
bash scripts/bootstrap.sh --dev
```

This builds the native tool/runtime, compiles one new Loom compiler, and
publishes `target/loom`, using LLVM O1 for this development rebuild.
`LOOM_BOOTSTRAP_COMPILER` overrides the preceding
compiler; a missing installed compiler uses the portable checked seed. This
short path does not compare stages. The no-argument command retains the full
stage 1/2/3 verification at default O2 for CI and bootstrap-boundary changes.
`LOOM_OPT_LEVEL=0..3` explicitly selects the native optimization level; this
never disables source integer checks or contract obligations. The runtime is
optimized even in Cargo dev builds, with dev assertions and overflow checks
retained, so managed Loom code does not call an unoptimized allocation layer.

`LOOM_TARGET_CPU=generic` emits the host architecture's generic CPU baseline
without detected host extensions. The default, `native`, keeps host tuning.
Neither selects another OS/architecture or changes arithmetic/contract checks.
CPU and feature selection participate in native object-cache identities.

The root Cargo workspace alone builds `loom-native` and the runtime, not the
public `loom` compiler. Invoking that compiler by path locates `compiler/std`
and `target/debug/loom-native` in its own checkout, so commands work directly
in an application's directory. `--std` and `--native-tool` select explicit
paths for other layouts. The local staging layout below also works without a
checkout; it is not a release package or stable compiler-artifact ABI. `--help` lists
each tool's command surface.

`loom test --no-run` produces the native test executable without running it;
`--output` optionally selects its path. Both forms of source tests and normal
test-only imports remain included. A package with no tests reports `0 tests`
without creating a binary. Ordinary `loom test` still compiles and runs its tests.
`loom test --recursive [directory]` visits packages in deterministic directory
order, including test-only packages and children of source-free directories.
Each package gets its own test scope and `target/tests` executable (`.exe` on
Windows); imported packages still exclude tests. Hidden directories, `target`,
`node_modules`, directory symlinks and nested modules with their own `loom.toml`
are not traversed. It continues after a package fails and exits unsuccessfully
if any selected package failed. `--no-run` checks and compiles the same selection;
recursive mode rejects shared `--output`/`--emit-ir` paths. An empty selection is
an error. Run nested modules separately; this is not dependency-test discovery.
`loom run [package] -- [arguments...]` passes arguments to the built program.
Try the [multi-package file tool](examples/wordcount/README.md) for a complete
edit, format, test, and run exercise.
The [core experience example](examples/core_experience/README.md) combines
checked construction, a dynamic concept contract, a Task, and lexical cleanup.

A failed native assertion reports the current test name and the assertion's
definition file, line and Unicode-scalar column, including assertions in called
helpers. The diagnostic is compiled into the executable: `test --no-run`
artifacts do not need source files at execution time. Successful test output is
unchanged. The first fault's message and test name survive cleanup, even if
cleanup also fails. Other faults do not yet carry source locations; this is not
a stack trace or a recover-and-continue test runner. Ordinary executables have
no test-entry instrumentation, and production builds exclude test-only names
and source contents.

## Relocatable local toolchain

After bootstrap, Node.js can stage and verify the current build in a new directory:

```sh
node scripts/stage-toolchain.mjs target/local-toolchain
target/local-toolchain/bin/loom check compiler/examples/wordcount
```

The layout is `bin/loom`, `lib/loom/loom-native`, `lib/loom/std`, and the adjacent
core and optional TLS runtime archives (`.exe`, `loom_runtime.lib` and
`loom_tls.lib` on Windows). CLI and editor commands
discover these relative to the executable, never the application directory.
The script compiles a generic-CPU frontend, copies the current Rust bridge/runtime
and source std, and exercises the relocated toolchain outside the checkout in a
path with spaces and Unicode. It checks formatting, check/build/test/run,
same-directory/embedded test isolation, real file I/O, forced GC, and editor
queries. Source checking and editor queries work with the native bridge absent.
Existing output directories are never overwritten.

Invoke `bin/loom` by path, or configure an absolute executable in VS Code and
leave `loom.stdRoot` empty. Bare-command discovery still requires explicit tool
paths; Loom does not guess which PATH entry actually ran. The staged toolchain
needs no Rust compiler to compile applications, but its copied native bridge
still requires this host's LLVM shared libraries and linker/SDK. The generic
frontend does not change the copied dependencies' CPU or OS requirements.
Use a staged compiler with `LOOM_BOOTSTRAP_COMPILER=/path/to/bin/loom` when
bootstrapping another checkout with the same checked-artifact interface.

To create a downloadable local archive from a verified stage, including Cargo
dependency notices and license texts, run:

```sh
node scripts/package-toolchain.mjs target/local-toolchain target/loom-toolchain.tar.gz
```

The script extracts the archive in another directory, checks and builds a real
application with its own std/bridge/runtime layout, and writes a `.sha256` file.
The archive contains [installation instructions](../distribution/INSTALL.md).
CI retains a local archive for each host platform as a workflow artifact. This
is not a self-contained LLVM distribution or an older-OS support guarantee;
formal release publication remains open.

## Windows bootstrap

The Windows x64/MSVC path passes the full bootstrap and native CI gate.
Use Rust 1.88, a Visual Studio developer environment, Git Bash, and an
LLVM 22 development package with `llvm-config.exe`, LLVM libraries, and
`clang-cl.exe`. The [CI recipe](../.github/workflows/ci.yml) provisions the 22.1.8
archive and supplies its missing `xml2s.lib` from a real static libxml2 build
using the static MSVC CRT, not a placeholder library. The Windows Cargo target
configuration and emitted program linker use the same static CRT. This matches
the LLVM package's allocator override; mixing dynamic-CRT allocation with its
message deallocator can crash even before IR lowering.
Bootstrap imports SDK library paths from the developer environment automatically.
Before standalone Cargo commands in Git Bash, run `source scripts/windows-env.sh`
to make those paths available to Rust's static-library packaging as well.
Native executables reserve a 32 MiB main stack, with the default commit size,
for bounded compiler traversals at O0 as well as optimized builds. This reserves
virtual address space; only used stack pages are committed.

A fresh checkout contains the same compressed, source-bound checked stage 0
used on Unix in [`compiler/bootstrap`](bootstrap/stage0.source). Git Bash verifies its
SHA-256 before decompression, and the current native bridge builds the first
Windows Loom compiler from it. No existing Windows compiler or cross-platform
file transfer is required. In Git Bash with the Visual Studio environment
inherited:

```sh
export LLVM_SYS_221_PREFIX='C:/llvm-22'
export LOOM_CC="$LLVM_SYS_221_PREFIX/bin/clang-cl.exe"
bash scripts/bootstrap.sh
target/loom.exe test compiler/examples/scalar
```

The native Windows bridge builds stage 0 from this checked input, then Loom
builds stages 1/2/3 and compares 2/3. The result is `target/loom.exe`; default
program/test outputs use `.exe`, library objects `.obj`, and the runtime archive
is `loom_runtime.lib`. Subsequent local edits use `bash scripts/bootstrap.sh --dev`.

## Bootstrap seed maintenance

The checked stage 0 is generated from the immutable source commit recorded in
`compiler/bootstrap/stage0.source`. The generator normalizes only
source-location prefixes, preserving every encoded byte length. macOS and Linux
CI emit it from that pinned source with the normal Loom type and
proof checker, then compare every byte with the committed input. To verify or
intentionally refresh it after a checked-artifact/backend ABI change, run on
macOS or Linux with a working compiler:

```sh
node scripts/bootstrap-seed.mjs --check target/loom
node scripts/bootstrap-seed.mjs --write target/loom
```

Refresh the pin only when the seed's source must change; ordinary edits to the
current compiler do not require a new checked input. The private checked format
is not a stable release ABI. `LOOM_BOOTSTRAP_COMPILER` and
`LOOM_BOOTSTRAP_INPUT` remain explicit overrides for trusted compatible inputs.
The active frontend remains Loom-only.

## Compiler latency

After rebuilding, measure the current macOS check/build path:

```sh
node scripts/benchmark-compiler.mjs
node scripts/benchmark-compiler.mjs --extended --sizes 10,50,200
```

The [harness](../scripts/benchmark-compiler.mjs) reports median wall time and
macOS peak RSS for scalar, data, and compiler packages, with raw samples in
`target/performance/compiler.json`. `--compiler`, `--output`, and `--runs`
select the binary, report, and sample count.

`--check-only --baseline path/to/previous/loom` compares two frontend binaries
against the same sources, alternating their order within each pair after one
warmup each. It records both binary hashes and per-sample wall/CPU time and RSS,
without compiling native artifacts or using object caching. Combine it with
`--sizes` to include generated package growth; `--compare-cache` is a separate,
mutually exclusive comparison.

Every sample starts a fresh process after one warmup; OS caches are warm.
The harness does not enable the opt-in object cache described below. Native decode, codegen, and linker
timings separate backend costs; remaining build wall time also includes
serialization and process/pipe overhead, not just frontend analysis. Peak RSS
is the operating system's reported maximum, not summed concurrent process
memory. `--extended` adds fresh-process startup (`--version`), actual
`test --no-run` compilation, and configurable generated package sizes. Growth
cases exercise multiple files, an imported package, records, generics and Lists;
the reported size is the helper count, not lines of code. Reports record input,
compiler, backend, runtime and harness hashes. Startup includes process-launch
overhead, and these synthetic packages do not substitute for large applications.

### Source name indexes

The compiler uses source `std.map` for package name indexes, retaining ordered
overload lists and independent query results. Ten alternating baseline/candidate
pairs on macOS arm64 (M4 Max, 2026-09-08) check identical current sources with
fresh O2 compiler processes and warm OS caches. Baseline `54d3a410` and candidate
`1c31f6dc` use the same native backend/runtime. The
[raw report](../benchmarks/compiler/results/2026-09-08-macos-arm64-name-index.json)
records both executable hashes, all samples, generated inputs and harness hash.

| Check | Linear index ms | Source Map ms | Change |
| --- | ---: | ---: | ---: |
| Scalar | 10.25 | 10.20 | -0.5% |
| Data | 6.08 | 5.80 | -4.6% |
| Compiler | 516.03 | 509.84 | -1.2% |
| 10 generated helpers | 11.86 | 11.80 | -0.5% |
| 50 generated helpers | 17.23 | 16.87 | -2.1% |
| 200 generated helpers | 35.88 | 32.55 | -9.3% |
| 512 generated helpers | 97.56 | 76.46 | -21.6% |

These are wall-time medians, not native build or application execution times.
The compiler check changes only modestly: total CPU medians are 500/495 ms and
peak RSS 222.20/215.82 MiB. The clearest gain is the larger single-package case;
its wall MADs are 0.68/0.84 ms. Sub-millisecond changes in small cases are not
broad speedup claims, and macOS CPU counters round those checks to zero.
This removes linear name scans, not all binding/checking costs or the need for
incremental frontend reuse.

### Native object cache

`build`, `test`, and `run` accept `--object-cache <trusted local directory>`.
The directory is created if needed; its parent must already exist. Omitting the
option disables reuse. For example, with the existing repository `target`:

```sh
target/loom build compiler/examples/data --object-cache target/native-cache
LOOM_NATIVE_TIMINGS=1 target/loom run compiler/examples/data --object-cache target/native-cache
```

With this option alone, every invocation still loads and checks source, including
contracts and selected dependency snapshots. Only the single object for the complete checked closure
is reused. Its key covers exact checked bytes, native-tool and loaded LLVM image
contents, effective target/CPU/features/layout, optimization and test mode.
It does not use an LLVM version string or file metadata as a content identity.
If the backend cannot identify its implementation, compilation proceeds without
caching. `--emit-ir` also uses full emission so the requested IR is produced.

Loom owns lookup and atomic publication under `objects-v1`. One binary bundle
contains the object and link metadata; SHA-256 covers both. Damaged entries are
misses. A verified object is copied into exclusively created staging before use;
executables always relink with the current runtime and linker. Library outputs
remain objects. Final outputs cannot be placed inside the cache-owned directory.
Trace output reports `loom cache: hit`, `miss`, or `unavailable`.

This is a trusted-local executable cache, not a source registry, attestation or
sandbox. Anyone who can replace a bundle can also compute a new checksum. Do not
use caches supplied by an untrusted checkout/download; filesystem ancestors and
the toolchain must remain trusted and stable during a build. Normal exits clean
owned staging; crashes can leave staging directories. Cache format changes may
discard reuse; no compatibility promise or automatic eviction is provided.
Full toolchain hashing and bundle verification have costs, so caching is opt-in,
not a promise that tiny programs build faster. Fine-grained frontend/proof reuse
and separate per-package objects remain future work. Bootstrap generation checks
do not enable this cache.

`node scripts/benchmark-compiler.mjs --compare-cache` compares uncached and warm
object-cache builds in alternating order. Initial misses are recorded separately;
the measurements include source checking, full toolchain hashing and relinking.

On Apple M4 Max/macOS 25.2, three alternating O2 pairs gave these medians:

| Build | Uncached | Warm object cache |
| --- | ---: | ---: |
| Scalar example | 107.29 ms | 192.61 ms |
| Data example | 92.87 ms | 185.51 ms |
| Compiler | 20,530.64 ms | 1,379.30 ms |

The compiler's peak RSS medians were 754.95/301.17 MiB; its source-only check
median was 499.72 ms. Cache hits had no decode/codegen phase and still linked.
Host variation was substantial: compiler uncached samples ranged 18.00–33.29 s,
hits 1.01–1.48 s. Its initial miss was a separate 15.29 s observation, not evidence
that a miss is faster than an uncached build. Small examples regressed, as the
cost of toolchain identity exceeded saved codegen. These are compilation timings,
not runtime kernel speedups or a portable performance guarantee.
[Raw samples and source/tool hashes](../benchmarks/compiler/results/2026-09-08-macos-arm64-object-cache.json)
retain the exact measurement basis.

### Frontend cache

`check`, `build`, `test`, `run`, and `emit-checked` accept
`--frontend-cache <trusted local directory>`. It is independent of
`--object-cache`; native commands can use both. For example:

```sh
LOOM_NATIVE_TIMINGS=1 target/loom check compiler/loom --frontend-cache target/frontend-cache
target/loom build compiler/examples/data --frontend-cache target/frontend-cache --object-cache target/native-cache
```

Every invocation reloads and parses the selected package closure and validates
its manifests, imports and locked dependency snapshots. A key covers the invoked
compiler executable's actual bytes, source paths/contents/membership, module
instances, import edges, standard-library trust and command mode. Production
build/run/emit-checked share an artifact; checking and isolated tests have separate
entries. Manifest edits that leave this validated semantic input unchanged need
not invalidate reuse. Source moves, changed dependencies, and new selected files
do invalidate it. Omitted dependency/test files do not become build inputs.

A whole-closure hit skips binding, type/effect/contract checking, compile-time
evaluation and lowering. After an edit, a separate last-successful definition
snapshot can reuse ordinary/generated abstract checks and concrete scalar/aggregate/List
and generic/compile-time-specialized bodies, including variadic instances.
Fresh bindings rekey types, static
arguments, calls and source locations;
changed definitions and overloads invalidate transitive consumers. Nominal/import
changes and syntax-producing macro or unsupported bodies recheck conservatively.
Within one check, each saved body's eligibility is computed once, and each
replayed concrete callee is validated and scheduled once in the current
environment. These lookup tables are rebuilt after every edit, never persisted
as evidence or reused across checking environments.
Declaration generation reruns before matching, using current build inputs.
Snapshots retain raw source and private expanded trees separately; unchanged
generated bodies use the current generating block's location and extent.
Variadic instances rebuild their current signatures and arity checks before body
reuse; expanded symbol IDs are not persistent identities.
Concrete methods can reuse bodies without skipping declaration/conformance
validation. Inherited contract locations track the concept's current file and
offset, separately from the implementation.
Async/Task bodies reuse checked flow before fresh coroutine lowering; task call
targets and creation labels use current bindings and source locations. Captured
frame bodies still recheck, rebuilding current capture plans. Their enclosing
declarations and ordinary callers can reuse evidence; nested source calls remain
transitive invalidation edges. No captured runtime environment is persisted.
Dynamic calls and boxes rebuild current interface/witness identities and used
method slots; cached bodies do not restore an old runtime dispatch table.
Scoped bodies reuse typed cleanup, while current transitive resource flow checks
still run. Implicit Dispose dependencies invalidate reuse just like explicit calls.
Build receipts use the freshly loaded project and actual artifact; requested IR,
linking and test execution still run.
Only successful checks are published. Each `checked-v3` entry contains metadata
and checked bytes under one SHA-256 checksum; `definitions-v2` binds its source,
private checked bodies and input metadata the same way. Damage causes a miss.
Snapshots preserve the loader's embedded-test selection: production builds omit
`test fn` bodies without losing definition reuse or importing test-only code.
Recorded `std.build.input_file` requests are resolved and their actual content
digests checked on every hit. Missing, changed or redirected inputs force a
fresh check. The receipt binds the snapshots used by that check or cache hit.

Use only a trusted local directory (with an existing parent), invoke the compiler
by its real executable path, and keep the toolchain/filesystem stable during a
build. An unidentifiable executable disables reuse; a checksum is not an
attestation against a malicious cache writer. No old cache-format compatibility
or automatic eviction is promised. `LOOM_NATIVE_TIMINGS` reports
`loom cache: frontend hit`, `miss`, or `unavailable`, and definition/body reuse
counts after whole-closure misses, plus a reason if a definition snapshot cannot
be exported. This is not a remote proof/artifact exchange.

One-shot CLI misses use `std.loom.checking.check_project_snapshot`: serialization
finishes before the mutable result escapes, then temporary cache references are
retired. Resident editor hosts use `check_project_cached`, whose private source
and body copies remain isolated from returned ASTs and programs. Both paths
keep the same invalidation, source-backed export and host trust checks.

Hashing has a fixed cost, so this remains opt-in. Measure a project with
`node scripts/benchmark-compiler.mjs --compare-frontend --check-only`; the harness
records the initial miss separately and alternates warm cached/uncached samples.
On Apple M4 Max/macOS 25.2, three alternating pairs measured:

| Check | Uncached | Warm frontend cache |
| --- | ---: | ---: |
| Scalar example | 9.93 ms | 22.14 ms |
| Data example | 5.07 ms | 19.11 ms |
| Compiler | 1,288.52 ms | 152.04 ms |

The compiler's peak RSS medians were 639.09/140.67 MiB; its initial miss took
1,328.82 ms. This is whole-closure `check` latency, not native codegen or program
runtime performance. Tiny packages regress because hashing exceeds saved work.
[Raw samples and source/compiler hashes](../benchmarks/compiler/results/2026-09-27-macos-arm64-frontend-cache.json)
retain the measurement basis; these are local observations, not platform targets.

Bootstrap generation comparisons and editor queries do not enable this cache.
To measure edits instead of unchanged whole-closure hits:

```sh
node scripts/benchmark-compiler.mjs --compare-edits --check-only --runs 3 --sizes 10,50,200
# Compare independently cached compilers on identical edits:
node scripts/benchmark-compiler.mjs --compare-edits --check-only --baseline /path/to/old/loom --runs 5
```

Each pair checks identical copied source after changing one contracted private
helper called from `main`. After warming each compiler/cache, a deliberately
false helper contract must reject before timing valid edits.
The cached variant must miss the whole-closure cache and reuse definitions and
bodies. Initial misses, alternating sample order, time, peak RSS and reuse counts
are recorded separately. This isolates a local edit; it does not represent a
public API change. Snapshot I/O and copying can outweigh saved checking, especially
for small programs; a positive reuse count alone is not evidence of a speedup.
Earlier result files used an uncalled probe; their timings are not directly
comparable to this reachable, contract-rechecked workload.

The [2026-10-02 paired comparison](../benchmarks/compiler/results/2026-10-02-macos-arm64-body-replay-edited.json)
measured the same edited inputs with two O2 compilers and independent caches:

| Edited package | Before | Per-check replay lookups |
| --- | ---: | ---: |
| Scalar example | 35.96 ms | 36.02 ms |
| Data example | 29.96 ms | 29.94 ms |
| Compiler | 1,749.70 ms | 1,614.22 ms |

The compiler case reused the same 1,841 definition checks and 2,982 bodies.
Avoiding repeated eligibility scans and callee reconstruction reduced latency
about 7.7% and peak RSS from 1,463 to 1,187 MiB (about 18.8%). These are nine alternating
fresh-process macOS arm64 medians, not arbitrary dependency changes or native
build speedups. A separate [uncached comparison](../benchmarks/compiler/results/2026-10-02-macos-arm64-body-replay-uncached.json)
measured 1,251/1,247 ms for the compiler, with no meaningful latency change.
The edited cache path still has substantial restoration, replay and writeback
costs; these measurements do not justify enabling caching by default.
Tracked files, explicit options and observed target properties bind both levels
of reuse. Backend objects are still whole-closure, not per-definition. Ordinary
compile-time execution cannot read arbitrary I/O.

### Earlier uncached measurements

Development snapshot on Apple M4 Max/macOS 25.2, medians of three warmed runs
on the same compiler source (not a portable performance guarantee):

| Operation | Before lookup/runtime changes | After |
| --- | ---: | ---: |
| Check the compiler | 1,974.58 ms | 159.12 ms |
| Check peak RSS | 77.25 MiB | 48.66 MiB |
| Build the compiler, O2 | 7,486.13 ms | 5,259.91 ms |

A separate same-source/runtime O1/O2 comparison reduced self-build time from
5,381.00 to 4,302.46 ms while the resulting compiler's check time stayed around
159 ms. This motivates O1 for the single-stage developer rebuild. LLVM remains
the largest self-build cost; these improvements do not substitute for future
incremental compilation or larger-project measurements.

On the LLVM 22 upgrade, the same-source O2 self-build measured 7.35 s with LLVM
19, 10.62 s with LLVM 22's default scheduler, and 8.07 s with a 32-candidate
scheduling budget (three warmed runs). The backend bounds this search without
disabling optimization or checked operations. Generated compiler self-checks
remained around 210 ms in a separate alternating comparison. This is evidence
for the compiler workload, not a guarantee for every generated program. Recheck
the budget on future LLVM upgrades; these tuning options are not a stable API.

Lexical temporary-root reuse on the same checked compiler input (`3903ae2d`)
reduces static temporary slots from 9,908 to 3,279. The largest per-function root
table falls from 302 to 82 entries. Controlled Windows-target O2 codegen reduces
the former largest-root function's stack allocation from 15,160 to 3,736 bytes.
These are IR/object measurements, not whole-process peak stack or on-runner
timings; local variables remain conservatively rooted for the function.

## Implemented subset

- Signed, checked 64-bit `Int`, `Bool`, scalar parameters, calls and recursion.
- Int bitwise `~`, `&`, `|`, `^`, `<<` and `>>`, also at compile time. Binary
  bitwise operands evaluate eagerly. Shifts require a count from 0 through 63;
  other counts fault. Left shift discards high bits, right shift sign-extends.
  Ordinary arithmetic still checks overflow. Bitwise operators bind more tightly
  than comparisons; shifts bind less tightly than arithmetic. See the
  [bitwise example](examples/bitwise/main.loom). Symbolic bitwise proofs remain
  unsupported and cannot satisfy required postconditions.
- IEEE binary64 `Float`, decimal/exponent literals, arithmetic and comparisons.
  Float division/remainder follow IEEE rules rather than integer faults; NaN,
  infinities and signed zero are retained. No implicit Int/Float conversion or
  fast-math reassociation is permitted. Scalar Float programs need no Loom runtime.
- `let`, `var`, assignment, final-expression returns, early return, `if`/`else`,
  `while`, `break`, `continue`, `assert`, and explicit `discard`. Boolean operators short-circuit.
- Parameter-type/arity overloads with explicit ambiguity errors.
- Named function values with structural `fn(Int) Int` types, contextual overload
  selection, generic specialization, and native indirect calls. Functions can
  be passed, returned and stored in aggregates without a wrapper allocation.
- Immutable records and tagged enums, nested exhaustive `match`, generic type and
  function parameters with inference or explicit arguments. Generic bodies are
  checked without hidden requirements; reachable instances use concrete layouts.
  Record field declarations require newlines or `;` separators, for example
  `record Point { x Int; y Int }`. `loom fmt` writes one field per line;
  bare spaces and commas cannot separate declarations. Semicolons do not
  terminate statements or separate constructor fields.
  Payload-free enums support equality within the same nominal type.
  Recursive data through `List` has a finite native layout; direct or mutual
  inline layout cycles reject.
- Structural tuples, positional projection, and `let`/`var` destructuring.
  Tuples work in generic types/functions, shared containers, `Result`, and
  compile-time results; they lower to ordinary native aggregate values.
- Postfix `?` unwraps source `std.result.Result`, or returns its error from the
  current function. Error types must match; the success value can then widen
  normally. Chained `??` and field selection work without a runtime protocol.
- Immutable UTF-8 `Text` and shared mutable `List[T]`. Copying a list binding
  shares its header: aliases observe growth and element replacement. Scalar
  records remain native values; managed fields retain their sharing semantics.
  List literals use `[a, b]`, with an optional trailing comma.
- Directory packages, private helpers and `pub`, package-wide explicit imports,
  and importer-scoped path dependencies. A simple `loom.toml` supplies the module name;
  no `src/` is required. A directory without a manifest can use its own files
  and `std`. Imported packages do not run initialization or their own tests.
- Both colocated `*_test.loom` and embedded `test fn`. Only tests can access
  test-only helpers. Production builds exclude test files and declarations.
  Identical test/production overload signatures are currently rejected.
- Source `std.int` provides `minimum`, `maximum`, decimal `to_text`, and
  `parse(Text) Result[Int, ParseError]` through ordinary imports and calls.
  `parse(text, start, end)` parses a byte range without a temporary slice;
  invalid or empty ranges return `ParseError.Invalid`.
  Parsing accepts ASCII decimal digits with an optional sign and leading zeros;
  invalid syntax and out-of-range input return errors, not arithmetic faults.
  It works in `comptime` without an additional intrinsic.
- Source `std.float` supplies `abs`, explicit integer conversion, finite/NaN queries,
  decimal parsing, and round-tripping formatting. `to_int` truncates toward zero
  and returns `NonFinite` or `OutOfRange`; `from_int` rounds ties to even.
  Parsing accepts complete signed ASCII decimals/exponents and exact `NaN`,
  `inf`, `+inf`, `-inf`, without whitespace or separators. Decimal overflow and
  underflow follow binary64 rounding. Grammar and errors live in Loom; narrow
  runtime codecs reuse Rust's numeric conversion, not a second source parser.
- Source Text operations include `starts_with`, `ends_with`, `find`, `contains`,
  `split`, `join`, and Unicode `trim`. Search offsets are UTF-8 bytes; an empty
  separator splits Unicode scalars. Join builds bytes once, not repeated concatenation.
- Source `std.option.Option[T]` provides `Some(T)` and `None`. List helpers include
  optional `first`/`last`, `is_empty`, shallow `clone`, in-place `reverse`, and
  `append`. Self-append copies the initial source prefix; clone detaches the outer
  list while preserving sharing of contained data. These are ordinary Loom functions
  and work at compile time, without additional intrinsics.
- Source `std.list.map`, `filter`, and `fold` accept typed callbacks. Traversal
  reads the initial index range once, in ascending order: callback appends are
  not visited, while changes to unread elements are observed. Map/filter return
  a new outer List and retain element sharing. Fold accepts a distinct accumulator
  type and returns its initial value for an empty List.
- Source `std.list.sorted(values, less)` uses stable merge sort over an initial
  snapshot, returning a fresh outer List with shared elements. It uses O(n log n)
  comparisons and O(n) scratch storage. The comparator must remain a consistent
  strict weak order; the compiler does not prove this requirement. Directory
  enumeration reuses this sorter instead of maintaining a separate algorithm.
- Source `std.map.Map[K Equal + Hash, V]` and `std.set.Set[T Equal + Hash]`
  provide shared hash collections over the existing List/record/enum mechanisms.
  Growth and clear preserve aliases; enumeration returns fresh outer Lists with
  shared elements and unspecified order. There are no map/set runtime intrinsics.
  See [hash collections](#hash-collections) for key laws and the minimal API.
- [`std.iter`](std/iter/README.md) defines `Iterator` with associated `Item` and
  `next()`. List/range sources and map/filter/take adapters are lazy; generic and
  dynamic consumers collect, fold, propagate errors with try_fold, or short-circuit
  with any/all. List iterators
  share fixed-view element identities and cursor state, not a copied snapshot.
  [The native example](examples/iterators) covers callback faults and cleanup.
- [`std.stream`](std/stream/README.md) supplies the async counterpart with typed
  `next().await`, generic/dynamic consumers and sequential Task callbacks.
  Short-circuiting retains the suffix; cancellation drains pending pulls without
  undoing their effects. [TCP chunks](std/net/tcp/chunks/README.md) use real socket
  readiness and fresh buffers while leaving closure explicit. See the
  [stream pipeline](examples/streams/README.md).
- Source `std.option` and `std.result` provide `map`, `and_then`, and
  `unwrap_or_else`; Result also provides `map_err`. The selected branch invokes
  its callback once, while the other branch preserves its payload without
  calling it. Arguments, including callback-producing expressions, still follow
  ordinary eager evaluation. These functions preserve payload sharing and work
  at compile time with pure callbacks, without new intrinsics.
- Source `std.text`, `std.list`, `std.result`, `std.file`, `std.fs`, and `std.io`. Private
  intrinsic signatures are checked against the runtime ABI and accepted only
  from the configured standard-library source root. Reading loops, UTF-8
  error policy, partial-write loops, and explicit file closure are ordinary Loom
  source. `file.write_text` creates or truncates a file; it is not an atomic or
  transactional write. `io.write_text` writes stdout without closing it.
- Source UTF-8 scalar helpers, shared byte buffers, integer rendering, Unicode
  property wrappers, stderr output, process arguments/exit, and direct child
  process invocation without a shell, with optional stdin input. Filesystem
  path and directory operations support source package loading. Native entry
  initializes argument access only when the emitted program needs it.
- `std.fs.create_dir` exclusively creates one directory; `rename` uses native
  same-filesystem replacement without deleting or copying first. `remove_file`
  and `remove_empty_dir` never recurse. Mutations return `Result[Bool, FsError]`
  with `Ok(true)` on success and distinct `AlreadyExists`/`NotFound` errors.
  `entry_kind` identifies the final symlink without following it; `kind` follows
  links. Unknown Windows reparse points are `Other`. Windows directory removal
  may remove a directory-link entry, while Unix rejects it; use `remove_file`
  for links. These path operations require trusted ancestors and promise neither
  race-free path confinement nor crash durability.
- `std.process.capture(arguments)` supplies stdin EOF and concurrently buffers
  binary stdout/stderr. `Result.Ok(Output)` preserves both streams for nonzero
  exits too; `Output.status` is `ExitStatus.Exited(code)` or `Terminated` when
  the platform supplies no exit code (for example, a Unix signal). Empty argv
  and spawn/read failures return `SpawnError`. Each result owns fresh buffers;
  capture does not echo them into diagnostics, impose a timeout, or define an
  ordering between streams. It buffers complete output, not a streaming API.
  `run`/`run_input` retain inherited output and report no-exit-code termination
  as `SpawnError.Terminated`.
- `std.process.capture_input(arguments, input Bytes)` adds binary stdin, with
  an optional third `Options` argument. Input is copied before workers start;
  stdin closes after writing while both output streams drain concurrently.
  Early child stdin closure preserves its output and exit status. No worker
  touches managed pointers, and no global signal disposition is changed.
- `capture(arguments, Options)` configures only that child: `directory Option[Text]`,
  `clear_environment Bool`, and ordered `environment List[EnvChange]`, with
  `Set(name, value)` or `Remove(name)`. The one-argument form uses defaults through
  the same implementation. Invalid options return `SpawnError.InvalidOptions`;
  an empty working directory is invalid, not the same as `None`. Clearing happens
  before edits; repeated keys follow host name rules (case-insensitive on Windows).
  Neither the parent environment nor working directory changes. Use an absolute
  executable path for deterministic selection with a changed cwd/environment;
  this API is not a process sandbox or a credential policy.
- `std.env.get(name)` returns `Result[Option[Text], EnvError]`: `None` means absent,
  while `Some("")` preserves an empty value. Each successful read is a fresh UTF-8
  copy. Empty names or names containing '=' or NUL return `InvalidName`; values
  not representable as UTF-8 return `Utf8`. Errors contain no values. This
  external read cannot execute at compile time and does not add global mutation.
- `std.bytes.get/set` index shared buffers with bounds checks and unsigned
  byte values (0–255). Both work at compile time. `decode_utf8` returns
  `Result[Text, Utf8Error]`, strictly rejecting invalid bytes with `Invalid`;
  `to_text` faults on invalid UTF-8. Both take an isolated copy on success,
  including embedded NUL, and decoding also works at compile time.
  `std.text.to_bytes(text)` copies the UTF-8 encoding into an independent mutable
  buffer; changing it cannot change the original Text or another encoding copy.
  `std.file.read_text` reuses the fallible decoder. Binary `read_bytes/write_bytes` preserve
  arbitrary binary contents and use the same source-owned read/write loops
  and explicit closure as text I/O. Both write APIs create or truncate a file;
  neither promises atomic publication or crash durability.
  [`std.file.lines`](std/file/lines/README.md) adds incremental UTF-8 line reading
  with `scoped` closure, explicit error items and the ordinary Iterator protocol.
  The [line-counting example](examples/file_lines/README.md) does not retain the
  whole file. Factory overloads of map/filter/take compose new scoped resources;
  the [file pipeline](examples/file_pipeline/README.md) stops at a selected prefix.
  [`std.stream`](std/stream/README.md) adds scoped factory pipelines with directly
  awaited resource borrows. General borrow-retaining adapters remain open.
- Source `std.io.read_bytes()` reads stdin to EOF; `write_bytes(Bytes)` writes
  stdout, and `write_error(Bytes)` writes stderr. These preserve arbitrary bytes,
  report byte counts/errors, and never close standard streams. `read_text()` still
  rejects invalid UTF-8. These whole-input reads buffer to EOF;
  `read_chunk(buffer, limit)` instead appends up to a positive byte limit and
  returns the count (zero at EOF), allowing interactive framed input without
  closing stdin. See the [binary filter](examples/binary_streams/main.loom).
- Source `std.hash.sha256.digest(Bytes) Bytes` produces a fresh 32-byte digest;
  `hex(Bytes) Text` hashes input and returns its 64 lowercase hexadecimal digits.
  Both also accept `(value Bytes, start Int, end Int)` to hash `[start, end)`
  without copying that input range. Empty ranges are valid; invalid bounds fault.
  The one-shot implementation uses fixed scratch storage and virtual padding,
  leaves the input unchanged, and also works at compile time within evaluator
  limits. Inputs must be shorter than 2^61 bytes. No hashing runtime operation
  is added; the library is neither a password hash nor an authentication scheme.
- Source `std.semver` parses SemVer 2.0.0 with `parse_version(Text)`, compares
  precedence with `compare(Version, Version) Int`, and parses/matches ranges with
  `requirement(Text)` and `matches(Requirement, Version) Bool`. Numeric components
  are not limited by machine integer sizes; build metadata does not affect
  precedence. The same source library is used by dependency resolution.
- Native executable builds when the selected package has `main`; otherwise,
  an object containing its public functions and their dependencies. Object
  symbols are private compiler conventions, not a supported foreign ABI.

The scalar example covers recursion, loops, a pre/postcondition pair, an imported
package, `std`, and both test forms. Native tests exercise overflow, division by
zero, short-circuiting, entry reachability, and test exclusion. Faults report a
brief reason and exit unsuccessfully. File errors use source-defined `Result`.
The [arguments example](examples/arguments/main.loom) parses external input,
validates it through a constrained `Count`, and sums the integers from one to
that count. `+0010` prints `55`; malformed or out-of-bound input reports an error
and exits unsuccessfully. Its colocated tests also exercise both boundaries.

A record initializer can use one final `..base` to fill fields not explicitly
initialized:

```loom
record Range {
    low Int
    high Int
}

let original = Range { low = 1, high = 2 }
let updated = Range {
    low = 10
    high = 20
    ..original
}
```

This constructs a new value; it does not mutate `original`. Explicit fields
evaluate once in source order, then the base evaluates once, even if every
field was replaced. The base must independently resolve to the same nominal
record declaration. Generic arguments can change if all retained fields fit
their new types; undetermined phantom arguments come from the base. Unknown
or duplicate explicit fields reject. Managed fields keep their normal sharing;
this is not a deep copy.

A refined record may supply base fields, but the new record does not inherit
its constraint. Use the existing checked boundary, such as
`OrderedRange(Range { low = low, high = high, ..original })`, to establish the
new invariant. Failed validation leaves the original value intact; other effects
of the field/base expressions are not rolled back. Required function contracts
still need proof. Constructing a new valid value supports multi-field changes
without exposing an invalid intermediate value.

Task-bearing bases transfer as whole values: all Task fields must still be
available, and replacing a live Task field cannot silently discard it. MustScope
record updates reject; use their explicit resource factories. NoSuspend operands
cannot cross a later await. Updates work at compile time and lower to ordinary
typed construction/projections without a new runtime operation. See the
[record update example](examples/record_updates/main.loom).

The [tuples example](examples/tuples/main.loom) covers heterogeneous results,
exactly-once evaluation, and shared fields:

```loom
fn pair[T](value T) (T, Text) { (value, "item") }
let number, label = pair(3)
let single = (true,)
assert single.0
let (nested_number, _), (flag,) = (pair(3), (true,))
var left, (_, right) = (1, (false, 2))
```

Tuple elements and the destructuring initializer evaluate once, left to right.
Tuple types are structural: element types and arity determine identity.
Existing tuple values require matching element types; aggregate widening is not
implicit. Contextual tuple literals can use the existing scalar weakening rules.
`(value)` is grouping, `(value,)` a singleton tuple; `()` remains unavailable.
Numeric projections are checked at compilation. Destructuring accepts nested
tuple patterns with exact arity at each level; outer parentheses are optional
for two or more elements. `let (value,) = single` binds a singleton's element.
All names enter scope after the initializer; `var` makes every bound name mutable.
`_` explicitly discards an element, never its initializer's effects; it cannot
discard Tasks or MustScope resources. Use `discard expression` for a whole value,
not `let _ = expression`. Enum binding patterns, scoped destructuring and
parallel reassignment are not implemented. Copying a tuple shares its managed
fields just as copying a record does; compile-time results construct fresh graphs
while preserving internal sharing.

`values...` expands a statically known tuple in a call, enum payload, tuple or
List literal. Each operand evaluates once, left to right; a callable receiver
still evaluates before its arguments. `(values...)` constructs the expanded tuple.
For example:

```loom
fn apply[A, B, R](callback fn(A, B) R, arguments (A, B)) R {
    callback(arguments...)
}
let pair = (2, 3)
let joined = (1, pair..., 4)
let values = [1, pair..., 4]
```

Expansion preserves shared fields and transfers every Task field once. It uses
ordinary typed arguments/projections, not runtime argument packing. A tuple
literal expands directly, retaining contextual inference and explicit comptime
arguments. Other tuple expressions use one saved snapshot. A later field can
fill a comptime parameter if an earlier field of that same snapshot remains a
runtime argument and the static field can be independently evaluated at compile
time. A static first field, runtime-bound tuple, captured function field, or
Task-producing source cannot use this path; supply those static arguments
explicitly. Lists have runtime-sized contents and cannot expand into a fixed
call signature. Empty tuple expressions still evaluate
once at their original operand position, including in zero-argument calls and
across suspension. They need no runtime pack. An empty expansion adjacent to a
comptime parameter must currently be bound first; specialization cannot erase its
runtime evaluation. A tuple of Tasks can be awaited directly; this uses
`std.task.all`'s input-order policy and preserves heterogeneous result types.

### Variadic functions

The common single-pack form declares one type pack among fixed type
parameters. Its width is the total type argument count minus all fixed
parameters; explicit arguments retain declaration order. Structural
tuple, function and nominal parameters may expand it at any parameter position;
direct variadic value parameters may have fixed parameters on either side.
All expansions of the declared type pack have the same width. Subtract fixed
parameters from the argument count and divide by the number of direct expansions;
a nonintegral width rejects. Ordinary inference checks matching element types
across every occurrence. Type patterns expand elementwise; each named value
pack is an immutable tuple.
Fixed prefixes/suffixes also work with empty packs, static parameters, contextual
function values and static/dyn methods; they keep source-order input effects and
ordinary Task obligations. See the [position example](examples/variadics/positions.loom).
The [repeated example](examples/variadics/repeated.loom) also exercises repeated
direct values, mixed static/runtime packs, nested iteration and Task frames.
Functions and concept methods also support independent type packs. Tuple,
callback and nominal input shapes, argument counts and expected results supply
affine width equations; only a unique nonnegative integral solution selects an
instance. Known widths locate middle parameters and nested expansions. Packs
used together in one elementwise expansion must have equal widths. Ordinary
inference still checks every element type and declared capability. Unresolved
partitions reject: group inputs in tuples or provide an expected result rather
than relying on a guessed split. See the
[independent example](examples/variadics/independent.loom), including coupled
equations, empty packs, dyn methods, CTFE and typed suspension.
Instance and body-cache keys retain every width, not just their sum. Shape
reasoning remains bounded; it does not enumerate specializations or solve all
integer feasibility problems. Impl headers use the same shape equations.
Iteration contracts use symbolic-width induction. Direct pack elements can also
supply declared scalar/inline aggregate concept method guarantees, without
assuming common concrete types or equal results from repeated calls. General type-dependent
induction remains unsupported.

```loom
pub fn pack[Ts...](values Ts...) (Ts...) {
    values
}

fn forward[Ts..., R](callback fn(Ts...) R, values Ts...) R {
    callback(values...)
}

async fn task_pack[Ts...](values Task[Ts]...) (Task[Ts]...) {
    values
}

let pair = pack(1, "two")
let empty = pack()
discard pack(empty...)
let callback fn(Int, Text) (Int, Text) = pack
```

Calls infer each element independently; explicit type arguments and contextual
function references select the same ordinary signature. A pack bound such as
`[Ts... Display]` applies to every element. Selection determines arity, then
checks the body with independent abstract element types and only the declared
bounds, before concrete instantiation. Zero-element packs produce an inferred
empty tuple, not a no-result expression; source `()` and `Unit` remain unavailable.

Pack-independent postconditions use ordinary abstract proofs, without sampling
arities. The compiler checks that the body, return type and contracts mention
neither the type pack nor any pack-bearing input; it then erases those inputs
for the required proof. Uncalled families and inherited method contracts are
checked too. Selected arities retain their normal bounds and resource checks.

```loom
fn keep[Ts...](value Int, context (Ts...)) Int
requires value >= 0
ensures result == value && result >= 0 {
    value
}
```

The [contract example](examples/variadics/contracts.loom) composes this guarantee
with constrained construction, CTFE and dyn methods. Element-independent
`comptime for` over a type pack, direct value pack or pack-bearing tuple input
also supports induction: a private counted loop uses symbolic nonnegative widths
and the existing scalar/List invariant rules. A structural tuple's extent is the
sum of its fixed fields and expansion widths, including repeated and independent
packs; nested fixed fields count once. Zero and arbitrary-many
iterations are proved, including index use, nested independent packs, early
returns and inherited method contracts. Native code remains statically expanded.

```loom
fn counted[Ts...](values Ts...) Int
ensures result >= 0 {
    var total = 0
    comptime for item in values {
        discard item
        total = total + 1
    }
    total
}
```

The [induction example](examples/variadics/induction.loom) also exercises shared
List updates, CTFE and dyn defaults/overrides. The
[structural example](examples/variadics/structural_induction.loom) adds fixed
fields, repeated/independent groups, elementwise expansions and global tuple
indices. This broadens input shapes, not the scalar invariant fragment. Element
observations through declared method contracts are described below; mixed
structural elements cannot borrow one pack's concept bounds. Type reflection,
pack-dependent result shapes and iteration over non-tuple structural sources
remain unsupported in family proofs. Shadowed erased sources and loop control that would change
its target reject. Executing a few selected arities is never a universal proof.

Family proofs can forward named pack-bearing inputs into another synchronous
family and compose its declared postconditions:

```loom
fn tuple_count[Ts...](values (Ts...)) Int
ensures result >= 0 {
    counted(values...)
}
```

The callee's universal proof remains mandatory, even with no concrete caller.
Fixed arguments retain ordinary evaluation, preconditions and effects; symbolic
widths and abstract element evidence stay in the private proof closure. Bare,
structural tuple and nominal patterns reuse the constructor schema used for impl
overlap checks. A bounded symbolic-word match jointly determines the callee's
type/sequence bindings across all inputs. Fixed fields, repeated/independent
packs and corresponding elementwise constructors retain their ordered schemas;
equal widths alone do not establish equal types. An unknown source word is not
split or sampled, and ambiguous or exhausted matches reject. Fixed type bindings
reuse header resolution, unaffected by same-spelled body locals or another
package's nominal declarations. Explicit type arguments participate in this same
joint match, for example `counted[Ts...](values...)`. Selected immutable type
aliases and structural types use ordinary type-value normalization; explicit
arguments cannot contradict the input shapes or split an unknown source word.
Bounds must follow from a bare source pack's
declared requirements; mixed captured sequences can forward only to unconstrained
packs. The
[forwarding example](examples/variadics/forwarding.loom) covers independent groups,
CTFE, callbacks, inherited dyn methods and shared List effects. The
[structural forwarding example](examples/variadics/structural_forwarding.loom)
adds fixed fields, jointly inferred groups, explicit type words and selected aliases,
nested constructors, nominal inputs and generic/default method scopes.
Overloaded/indirect/async callees, freshly assembled sequences, unresolved associated callee
patterns and fixed types with unknown expanded layouts remain unsupported in
family proofs.
Normal selected calls retain their existing checks and static expansion.

A structural parameter can supply arity without a direct value pack. This
includes a callback's parameter list or returned tuple, nested tuples and
nominal type arguments. An input with a known shape supplies arity; ordinary
inference then checks every occurrence, fixed field, element type and declared
bound. An overloaded callback still needs an exact contextual function type:
use explicit type arguments or bind it to an annotated function value first.

```loom
fn callback[R, Ts...](value fn(Ts...) R) fn(Ts...) R {
    value
}

fn apply[R, Ts...](value fn(Ts...) R, arguments (Ts...)) R {
    value(arguments...)
}
```

The [function-pattern example](examples/data_packs/functions.loom) exercises
callback wrappers, nested inference, contextual function references, CTFE,
shared captures and once-only argument evaluation through native commands.

The compiler elaborates each selected arity into ordinary type parameters and
native parameters, with a local tuple binding for runtime packs. There is no runtime
argument array, new LLVM operation, or hard-coded list of supported overload
sizes. GC sharing, compile-time evaluation, cleanup, and one-shot Task transfer
retain their normal checks. The [variadic example](examples/variadics/main.loom)
exercises these paths through check/build/test/run.

`comptime for item in values { ... }` visits an immutable, statically shaped
tuple or record binding. This includes the named value pack of a variadic
function, a structural tuple parameter, or an ordinary `let` aggregate. Tuples
use element order; records use field declaration order, not initializer order. Its body
is copied into a lexical block for each element and checked with that element's
type; body effects still run normally. Empty aggregates execute no iteration.
Names use ordinary lexical shadowing; mutable bindings and runtime-sized Lists reject.
This static form has no `break` or `continue` target: those statements use an
enclosing `while`, or reject if none exists. The
[pack iteration example](examples/pack_iteration/main.loom) covers mixed types,
effects and Tasks. [Record iteration](examples/pack_iteration/records.loom)
also covers generic records, nested maps, shared fields and captured callbacks.
Field reads retain normal visibility, refinement, resource and Task checks.
Scalar field copies do not mutate their source; shared containers keep sharing.
Refined records permit the same reads as explicit field access. Foreign private
records cannot be inspected, including empty records.

```loom
record Pair[A, B] {
    first A
    second B
}

fn fields[A, B](value Pair[A, B]) (A, B) {
    comptime map field in value {
        field
    }
}
```

A known generic record shape still checks with only its declared type bounds.
An unconstrained `T` is not assumed to be a record: select such code inside a
`comptime if` that determines its shape, for example with `std.reflect.describe`.
Iteration emits ordinary typed projections and lexical blocks, with no runtime
reflection table, boxing or implicit field mutation. It does not introduce
first-class type values or general macro expansion.

The optional two-binding form `comptime for key, item in values` (also available
with `comptime map`) binds a compile-time key: `Text` field names for records,
or zero-based `Int` positions for tuples and type/value packs. Both names are
lexically scoped to the body and must differ. Keys may feed `comptime`
parameters even when the corresponding value is runtime data. See the
[keyed example](examples/pack_iteration/keys.loom). Source
[`std.json.encode`](std/json/README.md) uses this to encode records without a
runtime reflection table; JSON policy stays in the library.

When the source name denotes a record or tuple **type**, `comptime for/map`
instead binds each field's type; no sample value is needed. The optional key is
still the record field name or tuple index. Value bindings take precedence over
type names. An unknown generic shape must be selected with `comptime if` before
iteration. The source is a single name, such as a generic `T`, a non-generic
record name or an immutable compile-time-selected type binding; it is not an
arbitrary type expression. Selected types use the same tuple/record shape and
private-field checks as named types. An inner value binding with the same name
retains ordinary value iteration. See the
[selected-field example](examples/type_values/selected_fields.loom).

`std.reflect.from_fields` assembles a visible record from a tuple in field
declaration order. An expected result type determines the record type:

```loom
import std.reflect.from_fields

record Flags {
    ready Bool
    visible Bool
}

fn enabled() Flags {
    let fields = comptime map Field in Flags {
        let value Field = true
        value
    }
    from_fields(fields)
}
```

Field count and types are checked normally, including safe weakening. The tuple
is evaluated once, aliases keep sharing, and Task transfer remains one-shot.
Refined record targets, foreign private records and MustScope resources reject;
the helper does not bypass construction constraints or cleanup. It must be
called directly, not captured as a callback. Both this helper and type iteration
lower to ordinary typed operations with no runtime schema dispatch. See
[type generation](examples/reflection/generation.loom) and source
[`std.json.decode`](std/json/README.md). Selected field and pack types also support
ordinary nominal constructors and record/enum patterns. The selected shape must
be known, possibly through `comptime if`; visibility, exact type arguments,
resources and constraints still apply. See the
[static nominal example](examples/type_values/static_nominal.loom). Macros and
first-class type values are separate facilities.

For a known function value, `std.meta.parameter_types(callback)` returns an
ordered compile-time `List[type]`, and `std.meta.return_type(callback)` returns
`Option[type]`. An omitted result is `None`, not a source `Unit` type. Both are
ordinary Loom library functions using structural packs; they do not execute
the callback, expose captured data, add an intrinsic or create a runtime registry.
Their type-valued results can be inspected inside `comptime`; a selected result
type may feed the usual annotation/construction boundary. An unresolved overload
still needs an annotation or explicit specialization. See the
[function-signature example](examples/type_values/signatures.loom).

The zero-argument overloads `parameter_types[fn(Int, Bool) Text]()` and
`return_type[fn(Int, Bool) Text]()` inspect a declared function type without a
callback value. Source `std.meta.Signature` supplies associated `Parameters`
(an ordered tuple) and `Output`; an ordinary impl family covers function types,
including zero parameters and omitted results. Other types need explicit library
conformance, and non-tuple `Parameters` reject during compile-time evaluation.
This is composable library metadata, not proof that an arbitrary conforming value
is callable. The callback overloads delegate to the same implementation. Neither
form adds compiler intrinsics, runtime type tags or native metadata helpers.

A structural parameter takes one tuple argument and one runtime tuple
parameter. It may repeat the same pack with fixed fields between expansions,
such as `values (Ts..., Bool, Ts...)`. If there are `k` expansions and `f` fixed
fields, the statically known tuple width `w` supplies `k * arity + f = w`.
Negative or nonintegral arities reject; ordinary inference then checks every
occurrence's ordered element types. This also applies to nested patterns,
function results and contextual function references. A zero-element pack retains
the fixed fields. Ordinary checking validates those fields and every expanded
element; `comptime for/map` visits the entire value tuple, while iteration over
`Ts` visits only its type pack. Several parameters may use the same pack, each
with its own fixed fields and element pattern:

```loom
fn choose[Ts...](left (Ts...), right (Ts...), first Bool) (Ts...) {
    if first {
        left
    } else {
        right
    }
}

let selected = choose((1, true), (2, false), false)
```

All occurrences must agree on arity and element types. Arity comes from explicit
type arguments, the final direct variadic argument count, or the first structural
tuple's type; an expected result can supply it when the inputs cannot. Ordinary
checking validates the remaining occurrences. This also works for contextual
function references. Supply explicit type arguments or a
prior typed binding when that first tuple needs contextual inference. Arguments
still evaluate once, left to right; there is no runtime shape dispatch. The
[shared pack example](examples/variadics/shared.loom) includes mixed structural
and direct packs, closures, sharing and Task transfer.

A pack need not occur in a value parameter. Factory functions can infer it from
their expected result, or use explicit type arguments for compile-time generation:

```loom
fn rows[Ts...]() List[(Ts...)] {
    []
}

let values List[(Int, Bool)] = rows()
let make fn() List[(Text,)] = rows
let explicit = rows[Int, Bool]
```

The same rule applies to concept defaults/overrides, dyn methods and async calls;
an async result is inferred inside its `Task` wrapper. An anchored result such
as `List[(Int, Ts...)]` can infer an empty pack from `List[(Int,)]`. Without type
arguments or an informative input/result, the arity is unknown and the use
rejects. A `type` result alone does not describe the types it will compute: supply
explicit arguments to such a generator. Explicit arguments take precedence over
context, and ordinary abstract body checking, element bounds and exact function
signatures still apply. See the [factory example](examples/data_packs/factories.loom).

`comptime map item in values { expression }` elaborates to an ordered typed
tuple of lexical block results. Each selected body runs once with its own
element type and normal effects. An empty aggregate maps to an empty tuple; for
nonempty aggregates each body must produce a value. `comptime for` remains a
no-result statement. Like `comptime for`, `comptime map` accepts an immutable,
statically shaped tuple or record binding, including the final value
pack of a variadic function. A record map also produces a tuple, not a record
with rewritten field types. Mutable bindings and other shapes reject.

A final `comptime values Ts...` parameter instead specializes each element:

```loom
fn constants[Ts...](comptime values Ts...) (Ts...) { values }
fn forward_constants[Ts...](comptime values Ts...) (Ts...) {
    constants(values...)
}
```

Elements use the ordinary static-parameter rules: scalars, immutable stored
aggregates/refinements, or function identities, including pure construction and
captured callbacks. The pack and
its `comptime for/map` iteration bindings preserve each element's static identity;
projections and forwarding need no runtime tuple snapshot. A runtime read of the
whole pack produces an ordinary tuple. Captured callbacks forward their current
environment, including through returned closures. Ordinary `let` copies remain
runtime bindings. Empty packs work without a special case at the call site.
`comptime values (Ts...)` also accepts one immutable tuple argument, including
fixed prefix/suffix fields around the expansion. Iteration over a static tuple
or record preserves the fields' compile-time identities, including nested loops
and closure capture. See the [static pack example](examples/variadics/static.loom)
and [aggregate parameter example](examples/comptime_parameters/aggregates.loom).

Unselected arities have not had their bodies verified, except for required
postconditions in the dependency-erasure and induction fragments above. Direct
`Ts... C` and `(Ts...)` element receivers may call synchronous concept methods
with explicitly declared postconditions and omitted returns or supported
scalar/inline aggregate results. Methods with omitted returns also compose declared heap
guarantees through the same call and loop rules. Loops propose the existing
entry-length comparisons for possibly resized storage even when its handle is
not reassigned; every backedge must preserve them, independently of call count.
Ordinary generic parameters,
method-local packs and static arguments reuse
method signature checking and width inference; unknown static values still cannot
supply proof facts. Fixed arguments and concrete tuples can select empty, mixed or
independent method-local packs without revealing the opaque receiver. Result types,
arguments, requirements and contracts cannot depend on `Self` or receiver identity.
Results compose through ordinary arithmetic, Text equations, helpers and loop
invariants;
each observation is fresh and opaque effects invalidate shared-storage facts.
There is no homogeneous-element assumption, sampling, or runtime proof object.
See the [observation example](examples/variadics/observations.loom), including
empty/mixed packs, generic and variadic methods, Text/tuple/record guarantees,
inherited bounds, nested independent packs, CTFE, function references and dyn calls.
Type inspection, opaque element escape and structural
element patterns remain unsupported for family proofs and reject even when
uncalled: proving selected arities is not a proof for every arity. Preconditions use fixed scalar
parameters, not the tuple pack, and retain ordinary checked/runtime boundaries.
Concept methods accept the same type pack, including defaults, overrides,
ordinary generic implementation parameters, structural inference and `async`.
Implementations may rename the pack and inherit its element requirements, but
cannot strengthen them. Conformance compares expansion-preserving type schemas,
not a few sampled arities. A selected arity becomes an ordinary method slot;
`dyn` dispatch retains only concrete slots used by the build. `Self` remains
receiver-only in dynamic methods, even inside an empty expansion. See the
[method-pack example](examples/variadics/methods.loom).

Impl headers accept type packs among fixed parameters. Target shape determines
each width:

```loom
record Cells[Ts...] {
    values (Ts...)
}

concept Collection {
    type Items
    fn items(self Self) Self.Items
}

impl[Ts...] Collection for Cells[Ts...] {
    type Items = (Ts...)

    fn items(self Self) Self.Items {
        self.values
    }
}
```

Fixed prefixes, nested/repeated tuple or nominal patterns and function types
use ordinary inference after expansion. Independent packs require a unique
nonnegative integral solution, just like function packs:

```loom
concept GroupView {
    type Left
    type Right
    fn left(self Self) Self.Left
    fn right(self Self) Self.Right
}

impl[As..., Bs...] GroupView for Groups[(As...)..., Bs...] {
    type Left = (As...)
    type Right = (Bs...)

    fn left(self Self) Self.Left {
        self.left
    }

    fn right(self Self) Self.Right {
        self.right
    }
}
```

Selected shapes check every method
and associated binding with abstract elements and declared requirements, not
only the caller's concrete types. `comptime for T in Ts` uses the same type
iteration as function packs. Method-local packs have their own width vector;
defaults, overrides, CTFE, dyn slots and native Task payloads stay ordinary.
Incremental recipes retain source identity and both width vectors, not generated
impl indices or summed lengths. See the
[single-pack example](examples/data_packs/implementations.loom) and
[independent impl example](examples/data_packs/independent_impls.loom).

Overlapping impl families reject before a call selects an arity. Fixed
constructors and anchored prefix/suffix patterns prove disjointness; unknown
intersections reject conservatively. Bounds or sampled arities do not prove
disjointness. An impl family's ordinary method contracts are proved at each
selected arity; unselected arities have not had their bodies verified.

General element-content/type induction remains open beyond declared method
observations. This implementation does not complete the accepted metaprogramming
design.

Records and enums accept type packs among fixed type parameters too. Independent
groups infer uniquely from structural fields/payloads or an expected type.
Explicit annotations preserve each declared pack slot with `...`:

```loom
record Groups[As..., Bs...] {
    left (As...)
    right (Bs...)
}

fn example() {
    let groups Groups[(Int, Text)..., (Bool,)...] = Groups {
        left = (42, "answer"),
        right = (true,)
    }
    discard groups
}
```

An unmarked tuple is one type; `((Int, Text),)...` is a group containing that
one tuple type. `()...` denotes an empty type group, not a source empty-tuple
value. Flat arguments cannot guess independent boundaries. Distinct width
vectors retain distinct nominal instances even when their flattened arguments
match. Cache replay, overloads, type comparison, recursive List-backed data,
impl families and Tasks retain those boundaries without a
runtime pack object. See the [group example](examples/data_packs/groups.loom).

The common single-pack form also accepts flat explicit arguments:

```loom
record Packet[Tag, Ts...] {
    tag Tag
    values (Ts...)
}
enum Tree[Ts...] {
    Leaf(Ts...)
    Branch(List[Tree[Ts...]])
}

let packet = Packet { tag = "data", values = (42, true) }
let leaf = Tree.Leaf(1, "leaf")
let tree = Tree.Branch([leaf])
```

An explicit type list, expected type, or structural initializer determines the
arity; ordinary inference checks the element types and repeated occurrences.
Nested data and function signatures can supply the shape. Phantom packs require
an expected type or explicit arguments. Empty packs, record updates, recursive
List-backed structures, compile-time values and Task fields retain ordinary
rules. Each selected arity checks field/payload requirements with abstract
elements, so concrete callers cannot supply undeclared capabilities. Unselected
arities are not verified. Instances retain the original nominal declaration;
no runtime pack or per-arity nominal type is introduced. See the
[native data-pack example](examples/data_packs/main.loom).

Pack elements can also project associated types: `(Ts.Item...)` and
`(Ts.Concept.Family[Ts]...)` expand with each receiver and its family arguments.
The pack must declare the required concepts; ordinary ambiguity, visibility and
family-domain checks still apply. Associated results do not identify their
receivers: infer `Ts` from another field/parameter, or provide explicit type
arguments. See the [associated pack example](examples/data_packs/associated.loom)
for compile-time evaluation, enum payloads and shared data across suspension.

Records also support `let Packet { value = item, .. } = packet` and the same
form with `var`. Named fields can reorder and nest record/tuple bindings; generic
arguments follow the initializer type. List every field or use `..` explicitly,
with the same discard restrictions. All bindings refer to one saved initializer,
and rebinding a `var` does not update the original record. Literal and enum
patterns belong in `match`, not ordinary bindings.

`match` supports nested enum and tuple patterns:

```loom
import std.option.Option
import std.result.Result

fn unpack(value Result[Option[Int], Text]) Int {
    match value {
        Result.Ok(Option.Some(number)) => number
        Result.Ok(Option.None) => 0
        Result.Err(_) => -1
    }
}
```

Arms are selected in source order. Exhaustiveness includes combinations of nested
variants; wholly covered arms reject. Inputs evaluate once. Whole-value fallbacks
and payload bindings preserve shared fields and one-shot Task obligations. The
[pattern example](examples/patterns/README.md) also exercises tuple patterns and
real async waits. Int, Bool, Text and Float literals also work at any pattern
position, such as `Some("ready", true, task)`. They use ordinary equality with
exact scalar types; Int does not implicitly match Float. `true` and `false`
exhaust Bool. Other scalar types require a binding or `_` fallback; refinement
predicates do not narrow this coverage domain. Equal numeric spellings denote
the same case, including `0.0` and `-0.0`; Float NaN reaches the fallback. Text
patterns compare decoded UTF-8 contents. Records use named patterns such as
`Packet { value = item, ready = true }`. Fields may reorder; separate them with
commas or newlines. Omission requires explicit `..` and cannot discard live Tasks
or unadopted resources. Matching a scoped receiver borrows its payloads instead;
omitted resources remain owned by the enclosing scope. Generic record arguments
follow the matched type.

Use `pattern if condition => body` for a Boolean guard. Pattern bindings are
visible in the guard and body. Only a matching pattern evaluates its guard;
false continues to the next arm, retaining effects already performed. Guards
never count toward exhaustiveness, including constant `true`; provide unguarded
coverage. The body can use proven immutable scalar facts from its guard.
Candidate Tasks stay available on a false guard; consuming them before a retry
or transferring them twice is rejected. Guards may await unrelated Tasks and
use ordinary block cleanup. Guarded matches on fresh MustScope resources reject
because adoption cannot be delayed until after a guard. Already-scoped resources
may be inspected with guards; unsuccessful guards do not transfer their payloads.

Expansion has a bounded decision budget; normal flat matches
retain their direct path. No runtime pattern engine or checked-artifact change is
needed, and compiler production sources do not adopt the new syntax.

## Multiline Text

Triple-quoted literals are raw `Text`, useful for source fixtures and expected
output without escaped newlines or quotes:

```loom
let expected = """
    record Point {
        x Int
        y Int
    }
    """
```

The opening quotes must be followed immediately by a newline. The closing
quotes start a line after optional spaces or tabs; that exact indentation prefix
is removed from each nonblank content line. A missing prefix is an error.
Whitespace-only lines become empty lines. Opening and closing boundary newlines
are excluded; leave an empty content line before the closing delimiter to retain
a final newline. LF, CRLF and CR line endings normalize to LF.

Backslashes, quotes and interpolation-like text are literal content. To include
a line beginning with triple quotes, use four or more quotes for both delimiters;
only a line starting with an equally sized quote run closes the literal.
Ordinary `"..."` literals retain their escape syntax. Both forms use the same
Text type, compile-time evaluation, matching and native representation.
`loom fmt` preserves multiline literal spelling and contents. See the executable
[multiline example](examples/patterns/multiline.loom).

## List construction

```loom
let values = [1, 2, 3]
let empty List[Int] = []
let nested List[List[Int]] = [[], [1, 2]]
```

An expected `List[T]` supplies the element type; otherwise the first element
determines it. Elements evaluate once from left to right. Each evaluation creates
a fresh outer List; contained shared values keep their identity. Empty lists
need a type context, such as a binding annotation, a function return type, or a
known parameter type. Supply explicit generic arguments when a call cannot yet
infer that context. There is no implicit numeric or container-wide conversion:
a fresh `List[Int]` literal may weaken individual constrained integers, but an
existing shared `List[Positive]` cannot become `List[Int]`.

Runtime literals allocate one header and, when nonempty, one backing store of
the known capacity. Generated code writes typed elements directly, without a
push/growth check per element. Compile-time literals use the same sharing rules;
materializing general compile-time object graphs still uses the existing
allocate-then-fill path to preserve aliases and cycles. `std.list.push` grows a
List; repetition syntax is not implemented.
`std.list.new[T](capacity)` creates an empty List with reserved element storage;
the no-argument overload retains ordinary on-demand growth. Reserved capacity
is not length or an observable part of List identity. The List can grow beyond
the reservation, and its aliases keep observing the same elements. Negative
capacity raises a RuntimeFault with ordinary Task fault handling. OOM remains
an uncatchable process-level fault, not a recoverable Result. Source `clone`
and `map` reserve their known output lengths.
An element block currently needs a value-producing path: an unconditional
`[{ return }]` is rejected even with a List type context. General typing of
non-returning expressions remains incomplete, as it is for tuples and bindings.

List and Bytes support `values[index]` and `values[index] = replacement`:

```loom
let values = [1, 2, 3]
let alias = values
alias[1] = 7
assert values[1] == 7
```

`let` prevents rebinding, not shared element updates. Indices are Int; List
updates check the element type and Bytes updates require an Int in `0..255`.
Negative or out-of-range indices fault. Receiver, index and replacement evaluate
once, left to right; final bounds checks follow operand evaluation, as in
`std.list.set`. Nested targets first evaluate their intermediate reads. Growing
an alias or triggering GC while evaluating an operand does not change the selected
container or leave a stale element pointer. The same operations work at compile
time and reuse direct native loads/stores without a new runtime dispatch layer.

Binding distinguishes `functions[i](value)` from `identity[Int](value)`; spelling
case is irrelevant. If an indexed function field and a generic concept method
both match, select `(holder.callbacks)[i](value)` or
`Concept.callbacks[T](holder, value)` explicitly. Text and Map indexing are not
provided; their source-library APIs retain explicit encoding and missing-key policy.

## Hash collections

`std.map` exports `new`, `length`, `is_empty`, `contains`, `get`, `insert`,
`remove`, `clear`, `keys`, and `entries`. Get returns `Option[V]`; insert/remove return
the previous value, or `None` for an absent key. Replacing a value keeps the
original key. `std.set` provides membership/count/mutation operations and
a `values` snapshot. Set insert/remove return whether membership
changed. Qualify zero-argument `new` when importing multiple container factories.

```loom
import std.map.new
import std.map.insert
import std.map.get
import std.option.Option

fn main() {
    let counts = new[Text, Int]()
    let alias = counts
    discard insert(alias, "Loom", 2)
    assert match get(counts, "Loom") {
        Option.Some(count) => count == 2
        Option.None => false
    }
}
```

Keys explicitly implement `std.equal.Equal` (`equals(self, other) Bool`) and
`std.hash.Hash` (`hash(self) Int`). Int, Bool and Text supply implementations;
Text compares and hashes UTF-8 bytes without normalization. Float deliberately
does not: ordinary NaN equality is not reflexive. Custom keys must provide an
equivalence relation, equal hashes for equal keys, and stable, side-effect-free
hash/equality behavior. Do not mutate key data affecting these methods while
stored. The compiler checks the concept signatures, not these algebraic laws.

Tables use open addressing, tombstones and geometric growth. Lookup/mutation
are expected amortized O(1) under a well-distributed hash, not worst-case O(1).
The built-in hash is deterministic and noncryptographic, without adversarial
collision protection or a persistent hash format. Enumeration scans capacity;
clear releases the table through shared state. Private nominal storage prevents
cross-package access to table internals. Snapshots isolate the outer List, not
shared keys/values; mutation is not thread-safe. No iterator invalidation or
concurrent-access protocol is introduced.

The [collection example](examples/collections/main.loom) combines user-defined
managed keys, shared List values, alias mutation and a compile-time-created map.
`loom test compiler/std/map`, `loom test compiler/std/set`, and the example use
the same source implementation; the native gate also runs the example with
collection before every allocation.

## Concepts

Concept conformance is nominal and explicit. `std.display` supplies `Display`,
implementations for Int, Float, Bool and Text, and generic `to_text`:

```loom
import std.display.Display

record Item { label Text }
impl Display for Item {
    fn display(self Item) Text { self.label }
}
fn render[T Display](value T) Text { value.display() }
fn optional[T](value T) Text {
    comptime if T implements Display { value.display() } else { "<value>" }
}
```

Bounds such as `[T Display + Rank]` are explicit requirements, including when
forwarding to another generic function. `optional` has no unconditional Display
requirement. Method ambiguity requires qualification, e.g. `Display.display(item)`;
import order does not select an implementation. Receivers evaluate once before
arguments, including in chained calls. The [concept example](examples/concepts/main.loom)
exercises these rules through the native CLI.

Each concept/type pair has one explicit implementation in the selected build
closure. Method signatures and bodies are checked even if unused, but only called
implementations are emitted. Static dispatch needs no boxes, method tables or
runtime registry. Methods retain source ownership in `std.loom.binding.Symbol`.
Test-only implementations cannot change unbounded production code; tests may pass
their explicit evidence to bounded generic functions.

Concept methods may provide a default body. An explicit `impl` is still required;
omitted methods use the default and matching methods override it. Defaults are
checked with abstract `Self` and the concept's declared capabilities, including
its associated types. They may call other methods, which select that type's
override. Static calls specialize directly; `dyn` tables retain only used slots
in closed executable builds. Concept-declared contracts are inherited by defaults
and implementations, which must prove their postconditions. An implementation
cannot add an undeclared precondition.

Implementation headers can declare type parameters and prerequisites:

```loom
record Wrapped[T] { value T }
impl[T Display] Display for Wrapped[T] {
    fn display(self Wrapped[T]) Text { self.value.display() }
}
```

All parameters must occur in the target type. Methods and associated-type
bindings inherit those parameters and bounds; concrete receiver types determine
the instance. Nested prerequisites, default methods and `dyn` use the same
conformance. Possible overlaps reject, including generic/concrete pairs and
implementations distinguished only by positive bounds; there is no implicit
specialization. Cyclic or exhausted conformance resolution reports an error,
not a negative `implements` answer. Test-only evidence retains its normal scope.

This slice supports nongeneric concepts and generic implementation targets.
Concept-typed parameter shorthand remains
later work.
Concept method contracts are checked against every implementation; extra
implementation preconditions reject.

Methods can declare their own type parameters, independently of the receiver:

```loom
concept Select {
    fn choose[T](self Self, first T, second T) T { first }
}
impl Select for Bool {
    fn choose[U](self Bool, first U, second U) U { second }
}
fn selected(source dyn Select) Int { source.choose[Int](1, 2) }
```

`source.choose(...)` infers method arguments; `source.choose[Int](...)` and
`Select.choose[Int](source, ...)` supply them explicitly. `Self` and enclosing
implementation arguments come from the receiver, not the explicit list.
Implementation methods inherit the concept method's parameter requirements;
they may rename parameters or restate requirements but cannot strengthen them.
Overloads still require a unique match. Defaults call the selected overrides.

Methods also accept the same `comptime` value and function parameters as ordinary
functions. The concept and implementation must mark the same parameter positions;
the receiver remains a runtime parameter. For example:

```loom
concept Adjust {
    fn apply(self Self, value Int, comptime extra Int) Int
}
impl Adjust for Bool {
    fn apply(self Bool, value Int, comptime extra Int) Int { value + extra }
}
fn adjusted(source dyn Adjust) Int { source.apply(40, 2) }
```

Static calls specialize directly. Dynamic calls use finite slots distinguished by
the method, its type arguments and its static values; static parameters do not
enter the runtime signature. Selected implementation/default bodies are checked
under their declared generic requirements, including known implementations in
unused functions with generic nominal receivers. An override does not instantiate
the default body it replaces. The [method example](examples/comptime_parameters/methods.loom)
combines defaults, recursive static callbacks, Text/Bool options and dynamic calls.
Bounded synchronous scalar concept method contracts are proved for each
implementation; required proofs may use the same declared postconditions through
abstract generic/associated receivers or `dyn`. Private summaries support Int,
Bool, IEEE Float, Text and inline record/tuple value facts, including scalar
refinements. Float uses the same bounded IEEE rules as ordinary calls, not
real-number algebra. Float equality remains a fact on the returned snapshot,
not a substituted definition: equal signed zeros can behave differently in
later arithmetic. Unspecified leaves remain independent; shared siblings
supply no content or alias facts. Summaries never guess an
implementation or enter native code. See the [generic](examples/concept_contracts/generic.loom)
and [aggregate](examples/concept_contracts/aggregates.loom) contract examples.
The [Float method example](examples/floats/methods.loom) composes dynamic calls,
defaults and method-local packs through these same summaries.
Bytes and List inputs can pass through these proofs, including generic nominal
receivers. List lengths use the stateful contract rules below; an opaque method
may change reachable lengths unless its postcondition establishes new facts.
Shared contents and unmodeled operations still provide no evidence.
Pure static and dynamic calls can also execute inside an isolated `comptime`
computation.

### Associated types and bounded data

A concept can name a type supplied by each implementation. Static instances
normalize projections into the existing concrete type and direct-call model:

```loom
concept Source {
    type Item
    fn item(self Self) Self.Item
}
impl Source for Int {
    type Item = Int
    fn item(self Int) Int { self }
}
record Cache[S Source] { source S; value S.Item }
fn cached[S Source](source S) Cache[S] {
    Cache { value = source.item() source = source }
}
```

`T.Item` uses the declared or branch-local concept evidence. If two active bounds
declare `Item`, qualify it as `T.Source.Item`. Incidental conformances on a concrete
argument do not change a generic declaration's choice. `Self.Item` refers to the
enclosing concept's member; implementation bindings may refer to other associated
members, with cycles rejected.

Record/enum parameters can carry bounds, including `[T C + D]`. Every use must
supply that evidence: `fn hidden[T](value Cache[T])` is invalid without `T Source`.
Interning a type in one scope does not authorize it elsewhere. Inference obtains
`T` from independent parameter/field positions, then checks `T.Item`; it cannot
infer a receiver from its associated result alone. Inputs still evaluate once in
source order. The [associated example](examples/associated/main.loom) covers
managed sharing, qualified projections and compile-time materialization.

Associated members may declare requirements and an optional default:

```loom
import std.display.Display

concept Source {
    type Item Display = Text
    fn item(self Self) Self.Item
    fn label(self Self) Text { self.item().display() }
}
```

`S Source` provides `S.Item Display`; multiple requirements use `+`. Each
implementation must establish those requirements for its effective bindings.
For example, an implementation binding `type Item = T` needs a declared
`T Display` bound; the promise being established cannot prove itself.
Defaults supply omitted bindings, while explicit bindings override them.
Abstract code cannot assume `Self.Item == Text` merely because Text is the
default. Default names resolve in the concept's defining scope, and `Self.Other`
uses the implementation's effective bindings. Cycles reject unless an explicit
override breaks them. Missing bindings without defaults remain errors.

Associated members may also take type parameters:

```loom
concept Family { type Item[T] }
impl Family for Bool { type Item[T] = List[T] }
record PairWith[X] { value X }
impl[X] Family for PairWith[X] { type Item[T] = (X, T) }
fn echo[S Family, T](source S, value S.Item[T]) S.Family.Item[T] { value }
```

Member parameters are distinct from the enclosing implementation parameters.
For `type Item[T Display] Display = T`, each application must establish its
input's `Display` requirement before normalization; implementations must prove
the result requirement for every admitted input. Bindings inherit the declared
input requirements, may rename parameters, and cannot strengthen those requirements.
Defaults and qualified projections follow the same rules as nongeneric members.
Normalization uses ordinary concrete layouts, with no runtime type functions.

Dynamic values still require explicit bindings for every nongeneric associated
member, including defaulted members; bindings must satisfy the member requirements.
Concepts with generic associated members cannot yet be used as `dyn` types.

### Dynamic values

An expected `dyn C` type implicitly packages a concrete value with its explicit
implementation evidence. The same methods work on static and dynamic receivers:

```loom
fn display_later(value dyn Display) Text { value.display() }
let value dyn Display = Item { label = "item" }
assert display_later(value) == "item"
assert render(value) == "item"
```

Bind associated types by name, for example `dyn Source[Item = Text]`. Binding
order does not change the type, but every member must be bound exactly once,
including members unused by methods. Boxing checks exact associated-type equality;
bindings are not covariant. Generic results may name dependent bindings:

```loom
fn erase[S Source](value S) dyn Source[Item = S.Item] { value }
```

Dynamic method parameters and results may use those bound associated types.
Default methods and generic callers share the same statically checked bindings.
The [binding example](examples/dynamic/bindings.loom) exercises managed results,
generic erasure and shared Lists without runtime type discovery.

Generic methods use one ordinary witness slot per concrete method instance
reached by this build. Even instances with identical native signatures remain
distinct when their type arguments differ. Unused generic methods have no slots;
there is no runtime specialization or type registry. This remains a source-package,
selected-build model: native objects do not promise arbitrary new generic
instances to a separately compiled consumer. `Self` outside the receiver remains
invalid for dynamic methods; method-local parameters are allowed.

The representation is a GC-owned concrete snapshot plus a read-only witness
table; copying a dyn value does not allocate another box. Contained lists retain
their ordinary sharing, while record value fields are copied. Static calls still
need no boxes. The [dynamic example](examples/dynamic/main.loom) covers escaping
receivers, heterogeneous lists, records/enums/tuples and allocating call arguments.

Closed native builds retain reachable witnesses and used method slots. Library
exports retain complete callable tables. No runtime type lookup or `any`
conversion supplies evidence. Cross-dyn conversion and concrete recovery
currently reject. A dyn-compatible method can use
bare `Self` only as its first receiver parameter; bound `Self.Item` projections
are allowed elsewhere. Static-only concepts may also use bare `Self` elsewhere.

Pure `comptime` code can construct, call and return dyn values, including methods
with associated bindings, generic parameters, defaults and static arguments.
The evaluator dispatches through the checked build's witnesses, not runtime type
discovery. Purity checking examines every possible checked target for a called
interface slot, including its body and contracts. An unexecuted runtime branch
cannot hide I/O; unrelated interfaces and unused method slots are not invoked or
included merely to evaluate another method.

Returned dyn values retain their admitted conformance and receiver snapshot.
Reification registers new output-queue witnesses and preserves shared/cyclic
containers, rather than retaining evaluation-local indices. Only methods needed
by the runtime program survive. The [compile-time dynamic example](examples/comptime_dynamic/main.loom)
exercises both computed scalar results and managed receivers used after compilation.
This does not permit runtime-local capture. Required proofs can use a declared
scalar postcondition of a synchronous `dyn` concept call, without inspecting its
concrete runtime receiver.

## Module dependencies

A dependency path is relative to the manifest declaring it; the key must match
the target module's name:

```toml
[module]
name = "app"

[dependencies.codec]
path = "../codec"
```

`import codec.parser.answer` selects the `parser` directory package in that
module. Each module can use its own direct dependencies; an application cannot
implicitly import its dependencies' dependencies. Only imported packages are
loaded, and only the selected root package contributes tests. Unused dependency
paths are not opened. Canonical roots are reused, without permitting imported
directory aliases to rename a package or cross nested module boundaries.

Package identity includes the module instance, not just its declared name.
Distinct roots with the same name can coexist through separate dependency
edges, and their nominal types remain distinct. Paths resolving to the same
canonical root reuse one instance. Display names do not grant access to another
instance's declarations or make its entry point/tests part of the selected root.

The [module example](examples/modules/app/main.loom) uses two independent `seed`
instances through separate dependency chains:

```sh
target/loom run compiler/examples/modules/app
target/loom test compiler/examples/modules/app
```

Path dependencies are editable source, not content frozen by a lockfile.

A dependency may instead select a Git repository, including a fork, directly:

```toml
[dependencies.codec]
git = "https://github.com/example/codec-fork.git"
rev = "0123456789012345678901234567890123456789"
subdir = "packages/codec" # optional; omitted or empty selects the repository root
```

The revision must be the full lowercase 40- or 64-digit ID of a commit, not
a tag, branch or abbreviated hash. Replace the illustrative URL/revision above
with a real module repository. The selected directory must contain `loom.toml`
with the matching module name; a monorepo need not have a root manifest.
`subdir` is an exact repository-relative path with `/` separators. Absolute paths,
dot segments, empty segments, trailing slashes and backslashes reject; spelling
must match the snapshot, including case. It cannot be combined with `path`.
Sources use credential-free HTTPS URLs with ASCII host/path spelling
and an optional port. URL escapes, query strings, fragments and IPv6 literals
are not supported in this first transport slice. `path` and `git` cannot mix.

Alternatively, select [SemVer 2.0.0](https://semver.org/spec/v2.0.0.html) tags:

```toml
[dependencies.codec]
git = "https://github.com/example/codec-fork.git"
version = "^1.2"
subdir = "packages/codec"
```

Use `rev` or `version`, never both. Tags are `v1.2.3` or `1.2.3`; monorepos may
also use `<subdir>/v1.2.3`. Lightweight and annotated tags resolve to full commit
IDs, and the selected module's `version` must exactly match the tag's SemVer
label. Conflicting aliases for one label reject. Module versions, when supplied,
must be complete SemVer values.

Ranges support caret (`^1.2`, also the meaning of bare `1.2`), tilde (`~1.2`),
wildcards (`*`, `1.*`, `1.2.x`), exact (`=1.2.3`), and complete-version comparisons
(`>=1.2.3, <2.0.0`). Commas intersect requirements; OR/hyphen ranges are not
supported. Caret respects `0.x` compatibility boundaries. Prereleases require
an explicit prerelease comparator with the same major/minor/patch. Build metadata
is ignored for range matching, but retained in the selected source identity.
A path dependency may add `version` to check its editable module's version.

Explicit resolution searches the selected import graph, including candidate-dependent
transitive imports. It minimizes duplicate instances within each URL/subdirectory
family, then prefers descending versions in sorted import traversal order. It
does not reward dropping unrelated dependencies. Disjoint requirements can keep
multiple instances; different forks/content are not unified by labels. Search can
be combinatorial for conflicting graphs; normal builds never run the search.

```sh
target/loom resolve path/to/app
target/loom resolve path/to/app --tests
target/loom check path/to/app
target/loom test path/to/app
```

Only `resolve` fetches or updates `loom.lock`; `--tests` includes the selected
package's test-only imports. Normal commands stay offline and fail if a selected
Git edge is missing, changed, or lacks an intact cached snapshot. Unused manifest
dependencies are not fetched. The lock preserves unrelated package edges, uses
location-independent owner identities, and anchors exact source identities to
SHA-256 snapshots, including each edge's selected subdirectory. Changing `subdir`
or a declared/effective requirement requires resolution even when the commit is
unchanged. Lock format 3 records the request identity, selected version, full
commit and digest; no old lock-format compatibility is maintained. Resolution
reports selected Git edge/instance counts and changed edges without echoing remote
tag data. Local path inputs remain
editable. Different Git URLs/commits/module directories remain different nominal
instances; identical module sources reuse an instance. Modules from one URL/commit
share a single verified whole-repository snapshot and content digest.
Path dependencies inside a Git source may select sibling modules but cannot
escape that snapshot. Selecting a subdirectory does not bypass validation of
other repository paths or import dependency tests.

Forks are local by default. The selected root module can explicitly select one
source for all **declared** edges with that module name, in the dependency itself:

```toml
[dependencies.codec]
git = "https://github.com/example/codec-fork.git"
rev = "0123456789abcdef0123456789abcdef01234567"
scope = "graph"
```

`scope = "local"` is the default. Only the selected root's `graph` entries set
graph-wide policy; imported modules' entries remain local to those modules.
This never grants an undeclared direct dependency, replaces `std`, or rewrites
another module's own namespace. A graph-selected path is relative to the root
manifest, including when replacing an edge inside a Git snapshot. A Git selector
retains its URL, exact commit and optional `subdir`; all matching edges reuse the
same nominal module instance. Each importing Git edge locks the effective source.
Changing that source requires `resolve` before offline use; local path sources
remain editable and are not made immutable by graph selection. An incompatible
fork must satisfy both original and root-selected version requirements and can
still fail ordinary type/contract checks. There is no compatibility proof based
only on its name or version, no implicit resolution during builds, and no
separate `replace` table.

Snapshots live under the root module's `target/loom-deps`. Every selected snapshot
is checked against actual regular-file contents and exact directory membership,
including unexpected files or empty directories, not a trusted sidecar. `resolve`
can repair a damaged cache from the same commit without changing its locked digest.
Raw Git blobs bypass checkout filters and hooks; symlinks, submodules, traversal
and nonportable/colliding paths reject. Executable mode is not restored: these
are source snapshots, not installed executables. Tree validation sorts once rather
than comparing every pair of repository paths.

The Git tool runs with isolated configuration/home and an allowlisted child
environment, HTTPS-only transport, verified TLS and no redirects or interactive
authentication. Remote stdout/stderr never enter diagnostics. `--git-tool` selects
a trusted executable; Git and host executable/DLL lookup paths must be trusted.
Resolution is anonymous unless `--git-credential-tool /path/to/helper` explicitly
selects a trusted, noninteractive executable implementing Git's
[credential-helper protocol](https://git-scm.com/docs/gitcredentials#_custom_helpers).
Loom invokes it with the single argument `get`, passing the exact HTTPS host and
repository path on stdin. There is no shell expansion, automatic discovery,
`store`/`erase` call or manifest-selected credential tool. The helper inherits its
ordinary host context and returns UTF-8 `username` and `password` fields; tokens
can be supplied as passwords. It owns any credential storage or refresh.

The resulting Basic authorization header exists only in the fetch/tag-discovery child's
environment, scoped to the selected URL, never in argv, a config file, lock,
cache or diagnostic. Redirects, external pack/bundle URLs and dumb HTTP are
disabled; TLS verification remains mandatory. This is not isolation from a
trusted Git executable or other processes able to inspect the child environment.
Helper failures and remote errors suppress their output. Exact-commit cache hits
and ordinary offline commands never invoke the helper. Explicit SemVer resolution
discovers tags again, even with cached commits. No credentials are needed to rebuild
an already resolved, intact snapshot.
Git-facing Windows drive/UNC paths use forward slashes without verbatim prefixes;
Loom's canonical paths and native cwd remain unchanged. Windows Git sessions
enable its builtin long-path support, not a promise about arbitrary external tools.
Native object and frontend caches are separate, opt-in trusted-local facilities
described above.
Resolution currently assumes a single writer and trusted filesystem ancestors;
atomic lock replacement is not crash durability or protection from concurrent
same-user mutation. Failed work leaves the prior lock unchanged, with best-effort
cleanup of owned staging directories.

## Function values

Use anonymous parameter types and omit the result for a no-result callback:

```loom
fn increment(value Int) Int { value + 1 }
fn apply(action fn(Int) Int, value Int) Int { action(value) }
fn choose() fn(Int) Int { increment }
fn main() {
    let action fn(Int) Int = increment
    assert apply(action, 4) == 5 && choose()(4) == 5
}
```

An expected function type selects an exact signature from overloads and can
infer generic arguments; `identity[Int]` explicitly specializes a named generic
function. Unresolved references require an annotation or type arguments. Calls
through values use their fixed signatures; function types are not covariant.
Original callee preconditions still execute at entry. `fn(Int)` has no result;
`fn(Int) Unit` is rejected just like an explicit Unit return on a declaration.

Function values work in records, enums, tuples and shared Lists. Arbitrary
callee expressions evaluate before their arguments, once each from left to
right, including `available()?(value)`. If a callable field and concept method
both match, use `(holder.action)(value)` or `Concept.action(holder, value)`.
Native function values use a managed environment and an entry pointer. Named
references have a null environment and allocate nothing; a private adapter
preserves the target's direct-call ABI. Only actual referenced targets enter
reachability, not every function with the same signature. Closed executables
derive indirect allocation effects from those targets, keeping scalar callbacks
free of GC roots and Loom runtime linkage. Library callbacks remain conservative.

Anonymous `fn(value Int) Int { ... }` and `async fn(...)` expressions use those
typed captured environments. Only referenced enclosing bindings are captured:
`let` retains its value and normal shared-data semantics; `var` shares one mutable
cell across the enclosing scope and all closure copies. Captures may escape their
creator and survive moving GC without Task scheduling or a runtime interpreter.
Scoped, MustScope, NoSuspend and live Task bindings cannot be captured. The
[closure example](examples/closures/README.md) covers nested captures, async
factories, compile-time results and lexical cleanup. Anonymous parameters must
have explicit runtime types; these literals do not declare their own generic or
compile-time parameters.

Pure compile-time calls and returned named function values use the same checked
signatures and contracts. Reification schedules the source declaration and its
type arguments in the destination program, not an evaluation-local function ID.
The [callback example](examples/callbacks/main.loom) uses source `std.list.map`,
overloaded callbacks, returned functions, and GC-stressed argument
ordering. Pure captured closures also execute and return from `comptime` blocks,
preserving shared/cyclic environments. Captured function `comptime` parameters
use the same environments, as described below. Bound method values remain later work.

## Contract boundary

Scalar constrained types use `type Positive = Int where self > 0` or
`type Money = Float where self >= 0.0 && self <= 1000000.0`.
`Positive(3)` yields `Positive` directly; a false constant is a diagnostic.
An unproved input evaluates once and returns source
`Result[Positive, ConstraintError]`. Widening `Positive` to `Int` emits no check;
arithmetic returns `Int`, while generic inference retains nominal identity.
`List[Positive]` never widens to `List[Int]`. The same boundary applies to Money:
`Money(10.0)` is Money, dynamic construction returns `Result`, and weakening to
Float needs no check. Money never implicitly converts to Int. Finiteness is a
predicate choice, not an extra hidden restriction on Float constraints.

Immutable Text uses the same boundary: `type Tag = Text where self == "ready"`
allows direct `Tag("ready")`, rejects a false constant, and checks an unknown
input once through `Result[Tag, ConstraintError]`. Widening Tag to Text needs no
check. Equality compares UTF-8 bytes without normalization.

A record can have a `where` predicate over immutable `Int`, `Bool`, `Float`,
`Text`, or nested inline record/refined fields. Shared siblings are allowed when
the predicate does not observe them:

```loom
record Entry {
    count Int
    notes List[Text]
}
type PositiveEntry = Entry where self.count > 0
```

Refinements also accept ordinary type parameters and explicit concept bounds:

```loom
record Entry[T] {
    count Int
    notes List[T]
}
type PositiveEntry[T] = Entry[T] where self.count > 0
```

`PositiveEntry(entry)` infers `T` from `Entry[T]`; explicit
`PositiveEntry[Text](entry)` is also valid. A parameter absent from the base needs
an explicit argument or an expected refined type. Predicates and pure helper
calls use those same arguments and declared bounds. Every specialization retains
the ordinary resource and mutable-observation checks: generic sharing is not
permission to constrain shared mutable contents. See [generic fixed-shape
views](examples/shared_views). Recursive invariant construction is a dependency
cycle, not an implicitly established invariant.

`notes` keeps ordinary sharing and mutation, including through existing aliases.
The predicate's `count` is an inline value, so those mutations cannot invalidate
it. Pure helpers may receive the whole record. Input-origin analysis follows
direct calls, recursive calls, preconditions, returned aliases and local
assignments to a fixed point across branches/loops. Immutable field projections,
enum tags and inline payloads are stable; mutable input contents are not.
Fresh local Lists/Bytes may be used as scratch storage, including through helpers
that also receive the input record. Record/tuple fields and enum payloads retain
separate origins: wrapping input and scratch together does not make the scratch
an input alias. Branches, record updates and returns merge these paths; an
analysis limit widens to unknown input storage, never to assumed freshness.
Copying or forwarding an input handle does not itself observe it. Named callbacks
and locally created closures retain these origins: every reachable target of the
checked function shape is analyzed, including its captures, preconditions and
returned aliases. Capture paths survive returned closures, nested callbacks and
inline field storage. Input-supplied callbacks stay opaque; their indirect calls
involving input storage, and mutation that stores such aliases into scratch,
conservatively reject. This analysis does
not prove predicate truth or protect arbitrary existing mutable aliases. See the
[effect example](examples/record_refinement/effects.loom).
The [callback example](examples/record_refinement/callbacks.loom) uses ordinary
higher-order helpers at the same checked construction boundary.
Scoped resources and one-shot Tasks cannot be wrapped in a refinement.
Its constructor checks an unknown value once and returns `Result`; copying the
refined value or widening it to the base record adds no check. Fields cannot be
assigned in place. A closed record
literal whose `Int`/`Bool` predicate is proved by the bounded verifier returns
the refined type directly. Record construction also preserves nested field
structure and stable scalar flow facts while treating unknown or effectful
fields as typed snapshots. An unobserved shared sibling does not prevent a
proof about known immutable fields; each real initializer still executes once,
in source order. The same snapshots and stable flow facts compose with verified
factory contracts. Unsupported evidence retains the checked boundary.
Contracted factories may carry unobserved values of any permitted input type;
there is no opaque-parameter type whitelist. Unmodeled enum values contribute
no tag or payload facts, and an unrelated Float supplies no finiteness or
reflexive-equality assumption.
Closed literals with `Float`, Text, tuple or enum fields can also
return directly when compile-time evaluation establishes its predicate; a false
constant is a diagnostic. Unknown inputs, calls, and failed optional evaluation
retain the `Result` boundary.
See the [record refinement example](examples/record_refinement/main.loom).

List-backed constraints can observe lengths independently of their element
type, including generic or shared elements. Content predicates may observe
immutable element fields even beside unobserved shared fields. Reads through
those fields into nested mutable Lists or Bytes reject, including a recursive
List with the same stored type as the outer header.
Construction must use a fresh List literal, a pure factory proved to return a
fresh unpublished outer header, or a proved unpublished local draft. Elements may retain their specified sharing;
the existing header cannot be returned through a nested input either.
Explicit source `std.list.clone` is one such factory, not a
compiler-recognized public name:

```loom
type PositiveValues = List[Int] where length(self) > 0 && all_positive(self)

fn checked_copy(values List[Int]) Result[PositiveValues, ConstraintError] {
    PositiveValues(clone(values))
}

fn build(count Int) Result[PositiveValues, ConstraintError] {
    let draft List[Int] = []
    var index = 0
    while index < count {
        push(draft, index + 1)
        index = index + 1
    }
    PositiveValues(draft)
}
```

Here `all_positive` is an ordinary pure function; see the complete
[List constraint example](examples/record_refinement/lists.loom). A proved
literal returns `PositiveValues` directly; other inputs keep the checked Result
boundary. Immutable aggregate elements use the same rule, including Text and
enum payloads; see the [literal example](examples/record_refinement/literals.loom).
The [shared-field example](examples/list_contracts/refinements.loom) constrains
account amounts while allowing their unobserved notes to remain shared and mutable.
The local draft keeps its actual header without copying. Existing origin and
checked call-footprint rules must establish that it was not published.
After construction, raw aliases and known captures may perform checked
non-publishing reads, including from `defer` and `scoped` cleanup. Immutable
local/field aliases to the actual header can also use the same complete-predicate
preservation proofs as constrained borrows: atomic stores and finite source
helpers must preserve the invariant after every store, not only on return.
Conditional may-aliases supply no assumed constraint. These proof-only borrows
do not change executable types or add checks; worker builds independently
revalidate mutation and withdraw stale content/extent observations.
Aliases cannot export the protected storage. Local copies retain their origins;
returning a raw handle, including an implicit aggregate return, rejects.
The analysis follows immutable local copies and record/tuple field projections
back to fresh storage. A builder may hold the fresh header directly in a nested
inline field; unrelated shared siblings retain their own permissions. The
construction's checked execution path through blocks, branches, matches and
eager expressions is analyzed separately. Alternatives are not treated as later
executions, and a return ends its path. Unrelated awaits do not expose a private builder. Pending
arguments/initializers still cannot transport raw aliases across publication.
Active lexical cleanup must satisfy the same non-publishing and preservation rules,
including fault-only exits. Loop-body publication uses a separate allocation
epoch when its fresh immutable binding is inside the iteration.
Raw aliases cannot flow into bindings outside that body or otherwise
escape; previously published headers are not the next iteration's draft.
Nested iteration bodies, early exits, cleanup and unrelated waits retain these
same rules. Loop-carried or cleanup-delayed mutable bindings may retain a header
when initialization and every assignment prove the same exact identity. This
also applies to one record/tuple projection while other fields change. The
original alias fixed point still tracks references propagated across iterations;
identity induction does not unroll a loop or identify fresh replacements. See the
[stable cursor](examples/record_refinement/stable.loom). Changing loop-carried
header identities, headers allocated outside a repeating publication, publication in
loop conditions and unsupported draft operations remain conservative.
Unknown predicates keep the ordinary `Result` boundary.
The [structural builder example](examples/record_refinement/structural_drafts.loom)
uses a generic record with a tuple field and an independently writable notes List.
The [iteration example](examples/record_refinement/epochs.loom) builds distinct
published headers across iterations, CTFE, cleanup and suspension.
The [raw update example](examples/record_refinement/raw_updates.loom) retains
sharing through preserving helpers, cleanup, checked rejection and a real wait.
Mutable bindings use private value snapshots: rebinding a variable does not
rebind earlier aliases or the published header. Structured `if`, `match` and
short-circuit joins retain the possible evaluated values without replaying
branch effects. All possible allocation origins must be fresh and unpublished;
branch-local exports reject even before the joined binding is initialized.
Fresh reassignment, inline aggregate copies and eager argument snapshots reuse
the same allocation and preservation rules; no rewritten locals enter executable
IR or caches. Equal may-origin sets do not equate independent choices or grant
predicates to their unselected alternatives. A preservation probe cannot combine
assumed predicates from distinct headers.
The [binding example](examples/record_refinement/versions.loom) checks once-only
effects, old aliases, cleanup, CTFE and real waits. The
[join example](examples/record_refinement/joins.loom) also exercises branching,
once-only conditions, cleanup and suspension. Loop-carried rebindings and delayed
mutable reads/writes retain their original slots and remain conservative, as do
aggregate arguments and captured mutating callbacks without a proved header identity.
`PositiveValues(existing_list)` still rejects when the input can have external
writable aliases or unsafe later raw uses. No automatic copy, ownership syntax or
runtime monitor is installed. Copies of a constrained value keep sharing its storage.

Indexing, ordinary non-escaping read helpers and explicit copies are allowed.
Shared element results may escape when their stored type graph proves they cannot
contain the protected outer header. This covers nested Lists and supported
record/tuple/enum fields without freezing their contents. Recursive backreferences,
opaque captures/dyn data and unresolved types remain conservative; non-escaping
helpers can still return independent scalar observations.
Extent-only predicates never grant an unrestricted outer alias.
When the predicate observes only length, element replacement is also permitted:

```loom
type Pair[T] = List[T] where length(self) == 2

fn replace_first[T](pair Pair[T], value T) {
    pair[0] = value
}
```

Aliases observe that replacement. Source helpers such as `set` and `reverse`
can also replace already constrained elements: `Pair[PositiveAccount]` preserves
each element's amount invariant by accepting a newly constructed
`PositiveAccount`, not a raw account with an unchecked amount. The
[element example](examples/list_contracts/refinements.loom) proves a positive
return after reading such an element and exercises replacement across aliases
and suspension while leaving unobserved notes mutable.
Source helpers
work by the same inferred effects, without special library-name rules or a new
check after each write. Appending additionally requires a static proof that
the predicate at length `n` implies it at `n + 1`, including helper preconditions
and arithmetic definedness. Thus `length(self) > 0` permits `push` and source
append helpers, while fixed length and upper bounds do not. Capacity/length
overflow faults before mutation. Unsupported proofs do not grant permission.
A content predicate can also admit an atomic replacement or append when the state
proof establishes the complete predicate after that store:

```loom
type PositiveElement = Int where self > 0

fn replace_positive(values PositiveValues, index Int, value PositiveElement) {
    values[index] = value
}

fn append_positive(values PositiveValues, value PositiveElement) {
    push(values, value)
}
```

The same array rules compose through immutable `Int`, `Bool`, `Text` and `Float` fields of inline records
and tuples, including nested paths and relations between fields. A bounded pure
scan may bind the current element locally and finish with an ordinary Boolean
expression, such as nonemptiness. Replacement, append and finite source helpers
must prove the whole predicate after every store. Unobserved shared siblings
remain mutable; the proof neither freezes them nor infers their contents.
Field equations are not whole-record equality or permutation evidence. See
[projections.loom](examples/list_contracts/projections.loom) for nested field
constraints and arbitrary-index write/restore contracts, and
[scalars.loom](examples/list_contracts/scalars.loom) for mixed scalar columns.
Text uses UTF-8 byte sequences; Float uses IEEE values, with storage equality
distinct from numeric equality. A restored Float still needs a non-NaN premise
to prove it numerically equal to its old value. Mutable element graphs remain opaque.

Indexing in a `where` predicate uses the same purity and storage-observation
rules as a pure getter, without requiring a wrapper function:

```loom
type PositiveHead = List[Int] where length(self) > 0 && self[0] > 0
```

Nested shared mutable storage remains inadmissible; the length guard also
ensures the indexed predicate is defined.

An atomic store composes the admitted List predicate with the stored value's type invariant
or literal value. Ordinary argument-preserving `set` and `push` forwarders use the same
rule, inferred from their checked bodies, not their names. Bounds faults precede
mutation. The proof does not replay arguments or change their evaluation order,
and adds no runtime predicate check. A callee's normal-return promise cannot
justify temporarily invalid contents. Unknown preservation, effects, raw alias returns and
publication into another aggregate reject at compile time. Factories
and borrows are checked through helper bodies, not trusted annotations. These
rules also run for compile-time code and unused concrete functions. The constrained
List has the ordinary native List layout and survives moving GC and Task handoff.
See the [generic extent example](examples/list_contracts/refinements.loom).
Atomic replacement and append also reuse stable caller evidence: `requires`, successful assertions,
selected branches, early exits and immutable scalar copies. Pure helper conditions
use the same bounded expansion as construction proofs:

```loom
fn replace_guarded(values PositiveValues, index Int, value Int) {
    if value <= 0 {
        return
    }
    set(values, index, value)
}
```

Mutable bindings, heap observations and effectful arguments supply no stable
caller facts. Arguments still execute once in source order; a failed guard or
bounds check precedes the write.

Finite source helpers can also borrow the representation for replacements and appends,
branches and supported loops. Their checked bodies execute in the same proof
engine, and the complete List predicate is required immediately after **every**
store, including stores before an early return or fault. Appends update both the
extent and contents in this algebra; a positive-head constraint can admit a
negative tail, whereas an all-positive constraint cannot. For example:

```loom
fn fill_positive(values List[Int], value Int) {
    if value <= 0 {
        return
    }
    var index = 0
    while index < length(values) {
        values[index] = value
        index = index + 1
    }
}
```

`fill_positive` accepts `PositiveValues` without a copy or repeated runtime
predicate checks. The proof infers the continuing predicate at loop heads and
checks each store inductively; a callee's return contract cannot replace that
check. Borrowed headers still cannot escape or be published. Unknown effects,
unsupported bodies and mutating fault cleanup reject conservatively.
Strengthening pre-existing writable aliases remains later analysis work.
Shared builds with admitted content writes no longer treat that nominal type's
contents as read-only; old-cell equality still requires interference-safe proof.
Admitted appends also withdraw fixed-extent evidence across all aliases. A
sampled size/content guard cannot reserve room for a later shared append.
Multi-step helpers are re-proved with interference before each source access.
The continuing predicate describes a primitive store's atomic transition, not
stable values across accesses: copying a previously read cell can be sequentially
preserving but rejected with workers.

An explicit conversion between Int refinements can also return the destination
directly when the source predicate proves the destination predicate, including
the absence of overflow in that predicate:

```loom
type Positive = Int where self > 0
type NonNegative = Int where self >= 0
fn widen(value Positive) NonNegative { NonNegative(value) }
```

The input still evaluates once. Call-free Int and Float refinements also reuse
identical predicates or conjuncts of an already true `&&` expression:

```loom
type Bounded = Float where self >= 0.0 && self <= 100.0
type NonNegative = Float where self >= 0.0
fn widen(value Bounded) NonNegative { NonNegative(value) }
```

The latter rule matches typed expressions exactly, permits regrouping/reordering
conjuncts, and does not apply integer algebra to IEEE values. It cannot extract
a condition hidden behind `||`. Unproved narrowing and exhausted or unsupported
proofs retain ordinary checked `Result` construction. IEEE comparisons also use
the bounded value/contract rules in the [contract boundary](#contract-boundary).
The IEEE order theory also proves wider constant ranges and comparison chains,
without treating numeric equality as identity of arithmetic computations.
Unresolved supported arithmetic uses the binary64 SMT theory described below,
not integer or real algebra.

Construction also consumes already established facts about an immutable scalar
local or parameter, from `requires`, a successful `assert`, or the current
`if`/`while` branch. For a pure helper predicate, it transfers only those stable
scalar facts into a private proof scope and checks the helper's requirements and
evaluation safety. Unknown results retain checked construction; caller effects
are not replayed, and proof-only helpers do not become native roots.
The established condition itself may call a bounded pure scalar helper, such as
`requires !is_nan(value)` followed by `NonNegative(abs(value))`. Its actual
selected body is expanded, including guarded requirements and successful
evaluation checks; an opaque candidate supplies no evidence. This also applies
to assertions, branch/exit guards and exact immutable copies. Check-only call
identities remain separate from executable reachability, and concrete type and
compile-time arguments remain part of the selected instance.

Int, IEEE Float and immutable Text use their own value theories. Text equality
and immutable copies do not expose native addresses or make mutable bindings
stable; unknown alternatives remain unknown. See the
[Text construction example](examples/text_contracts/construction.loom).

```loom
type Positive = Int where self > 0
fn checked(value Int) Positive requires value > 0 { Positive(value) }
fn selected(value Int) Positive {
    if value > 0 { Positive(value) } else { Positive(1) }
}
fn copied(value Int) Positive {
    assert value > 0
    let copy = value
    Positive(copy)
}
fn guarded(value Int) Positive {
    if value <= 0 { return Positive(1) }
    Positive(value)
}
fn related(value Int, lower Int) Positive
requires value > lower && lower >= 0
{
    Positive(value)
}
fn retained(value Int) Positive {
    let copy = value
    assert value > 0
    Positive(copy)
}
```

The original boundary condition remains; these constructors add no second check,
even before LLVM optimization. Immutable Int bindings retain scalar initializer
equalities; later assertions can therefore establish an earlier copy's bounds.
Bounded integer difference propagation combines relations such as `x > y` and
`y >= 0`, including constant offsets and copy chains. It uses mathematical
integers internally, but a newly evaluated goal must still be proved within
machine range. Original initializer arithmetic and its fault checks remain.
Float copies inherit only exact predicates established at the copy.
After a standalone `if`, exactly one
continuing branch can supply its guard, including guards ending in `return`,
`break` or `continue`. This retains the surviving condition, not assertions made
inside a branch or deferred cleanup. Facts use binding identities, not names.
This slice excludes `var`, heap reads, general relational solving,
and facts inferred by joining two live branches. Literal Float predicates can be reused, but
`!(x > 0.0)` does not prove `x <= 0.0` because of NaN. Unknown cases still return
`Result`; calls and input expressions are never duplicated to seek a proof.
See the [relational construction example](examples/relational_constraints).

Predicates over `Int`, `Float` or `Text` may call ordinary pure helpers, including helpers with
loops, recursion, and freshly allocated data:

```loom
fn positive(value Int) Bool { value > 0 }
type Positive = Int where positive(self)
```

The supported transitive operation/call closure is checked even for unused
constrained declarations; I/O and external mutable inputs are forbidden.
The [bounds example](examples/data/bounds.loom) exercises direct constant
construction and runtime `Result` checks with pure helpers. A known true
predicate removes the check; known false rejects. Unknown inputs or unsuccessful
optional evaluation retain the single runtime construction boundary. A fault
during optional folding is not proof of validity or rejection; ordinary runtime
fault behavior remains. Explicit `comptime` faults still reject compilation.

Refinement-to-refinement implication also expands direct, acyclic scalar helpers
with local bindings/reassignment, `if/else` and early body returns. It uses each checked
specialization in its defining scope and preserves eager arguments, unused
calculations, short-circuit guards and helper preconditions as proof obligations.
For example, replacing `self >= 0` above with `nonnegative(self)` can still remove
the check when `nonnegative` returns `value >= 0`. A helper returning `true` after
`let unused = value + 1` cannot discard a possible overflow. Unsupported helper
loops/shared-storage mutation, indirect calls or exhausted expansion retain runtime checks.
General implications over shared mutable contents remain open; supported
immutable input invariants and flow facts are described below.

`requires` is checked before the callee body. Every declared `ensures` must be
proved; unknown or unsupported proofs reject the build, including for functions
outside the emitted entry closure. There is no runtime postcondition fallback.

When fast rules leave a supported postcondition unresolved, the CLI submits its
path assumptions and conclusion to Z3. One counterexample query combines the
function's unresolved exits. Only `unsat` establishes a proof; `sat`, unknown,
the two-second solver timeout, encoding limits, launch failure or malformed
output cannot pass. Z3 composes Boolean conditions, integer feasibility
and byte-sequence equations, including Text cancellation and length/content
relations; see [SMT contracts](examples/smt_contracts/main.loom). Sequence lengths
count UTF-8 bytes, not Unicode code points. Source overflow, concatenation size
and modeled access safety remain separate obligations: the solver must establish
them under the actual evaluation guards as well as the final truth. For example,
`length(value) == 0` makes `value == "" || number + 1 > number` safe even for
`Int`'s maximum, but cannot make an eagerly evaluated helper argument safe.
Hypothetical operations never supply their own successful-check assumptions.
Non-affine integer `+`, `-` and `*` remain symbolic terms over the same logical
values. The [polynomial example](examples/smt_contracts/polynomials.loom) proves
square nonnegativity, square expansion and difference-of-squares identities, with
overflow obligations, immutable Text lengths and verified call summaries.
Signed interval transfer composes integer `+`, `-`, `*`, `/` and `%` before SMT, using
mathematical endpoints and a bounded traversal. It discharges known scalar ranges
and overflow safety without discarding eager operand checks; loose ranges remain
unproved. The CLI also enables Z3's bounded polynomial normalization before search,
without extending the solver timeout. Signed symbolic `/` and `%` use mathematical
magnitudes and a quotient truncated toward zero, not SMT's signed `div` directly.
Their zero and `Int.min / -1` fault guards are independent of result bounds,
including `%`; only an actually executed normal continuation supplies them.
Fixed-sign divisor intervals use truncating quotient endpoints and remainder
sign/magnitude bounds. Zero-containing or wider quotient intervals stay unproved
locally. The [range example](examples/smt_contracts/division_ranges.loom) checks
without Z3; mathematical endpoints do not erase source faults.
See [division contracts](examples/smt_contracts/division.loom). Universal pack
content/type proofs remain unsupported. Nonlinear solving is incomplete: unknown or timeout
still blocks a proof.

`std.text.byte` joins this algebra as an unsigned UTF-8 byte observation.
Normal returns establish 0–255 locally; hypothetical reads must independently
prove `0 <= index < length(value)`, even for a reflexive equality. Immutable
Text equality and concrete UTF-8 contents compose through the same SMT byte
sequence, not a code-point or signed-byte model. See the
[byte contracts](examples/smt_contracts/text_bytes.loom).

Signed 64-bit `~`, `&`, `|`, `^`, `<<` and `>>` compose in the same proof model.
Nonnegative mask bounds and XOR factor parity prove locally; other supported
relationships use exact word operations in SMT, with arithmetic right shift.
Shift counts independently require 0–63, and neither masking nor cancellation
erases an eager operand fault. See [bit contracts](examples/smt_contracts/bitwise.loom).

Constrained construction, scalar flow facts, pure-helper implication and supported
List-append preservation use this same backend after their fast rules fail.
Helper evaluation safety and predicate truth are discharged together, before
removing a boundary check. For example, `self * self == 1` implies
`self == -1 || self == 1`, and a zero byte length implies empty Text content;
see [refinement examples](examples/smt_contracts/refinements.loom). Unknown or
failed optional proofs retain checked `Result` construction or reject an
unproved mutation; they do not turn into mandatory postconditions. A helper
whose final Boolean is true still needs its eager arithmetic to be safe.

`std.loom.proof.ProofBackend` is an explicit trusted host callback, supplied to
`std.loom.checking.with_proof_backend(inputs, backend)`. The default public checker
does no process I/O and retains the in-process proof fragment. A backend and
imported trusted caches belong to the same host trust boundary; they are not
source-program axioms. The compiler's Z3 process is absent from emitted programs.
The public `implies`, `implies_facts`, `implies_with_helpers` and
`preserves_list_append` queries also accept an explicit backend; their default
overloads remain I/O-free.

Proof composition uses the checked expression model, not a second executable IR.
A closed [operation description](std/loom/proof/operations.loom) supplies operand
shape, eager/conditional evaluation, heap observations and totality. Shared
traversals handle expansion, choices, entry guards and operand-definedness;
[primitive theories](std/loom/proof/primitive_theories.loom) interpret values and
check operation-specific obligations. Unknown operations are not implicitly pure
or total. Correlated choices reuse their exact guard without increasing proof
budgets; independent guards stay independent. Algebraic equality never permits
reordering effects or erasing eager faults. This is compositional but bounded,
not a complete solver for arbitrary contracts or a user-axiom mechanism.

Established affine equalities share a bounded, fraction-free elimination basis.
For example, `a + b == total` and `b + c == total` prove `a == c`, including
when those quantities come from fields, verified call summaries, Text lengths
or captured List lengths. Positive cross multiplication preserves comparison
direction without rounding or machine overflow. This algebra does not prove
equal contents from equal lengths or preserve stale heap observations. Each
source arithmetic operation must still be proved defined; cancellation cannot
hide an overflowing intermediate. Rank, coefficient-size or work exhaustion
leaves this fast path unresolved. Nonlinear terms and the bounded quantified
fragment below use SMT, not the affine fast path.

Linear inequalities also compose by bounded variable elimination: for example,
`a + b <= limit` and `b >= reserve` establish `a <= limit - reserve` when
the subtraction is defined. The same rules apply to immutable scalar observations
of Text and Lists. Only positive scaling and addition combine inequality rows;
strict integer comparisons retain their unit gap. A contradiction between the
established premises and the negated goal supplies the proof. Disjunctions and
disequalities are not silently split into independent bounds. This is not a
complete integer-feasibility solver; unresolved representable postconditions
can use the SMT backend. Storage invalidation and source overflow checks still apply.

Function contracts can reuse direct, acyclic helpers over scalars and inline
records/tuples, with local bindings/reassignment and conditional bodies. The [contract example](examples/contracts/README.md)
shows a predicate with its own `requires`: proving its returned Boolean alone is
not enough; the caller must also establish that requirement. Expansion preserves
multiple parameters, branch/short-circuit guards, eager arguments and unused
arithmetic. Scalar choices normalize into bounded Boolean clauses for contracts;
body-call proofs reuse typed branches. Assertions inside a helper are obligations
in a postcondition, not assumed facts.
Local assignments replace symbolic values without changing earlier observations.
Expression blocks and short-circuit operands preserve evaluation order and merge
only executed local writes. Overwriting a calculation does not erase its fault
obligations. This does not admit shared-storage mutation or general helper loops.
Helpers used only by `ensures` do not enter native reachability. Their actual
bodies and own contracts are still checked in their defining scope, including
when the caller is unused. Verified postconditions retain call-free checked
expressions for compile-time execution.

Direct calls in a body requiring proof use the callee's verified postconditions
as a summary for `Int`/`Bool`/`Text` values and leaves of inline records/tuples.
Synchronous `dyn` calls can use the exact concept
method's declared scalar contract, which every implementation must satisfy;
the proof never guesses a concrete witness. For example, `identity(value)` with
`ensures result == value` lets a forwarding function prove the same contract.
Without a summary, the prover can expand a finite pure body using those values. Arguments
are evaluated in order and their values captured before applying the summary;
separate call results are not equated merely because they share a callee. The
emitted function keeps its ordinary calls and original locals.
Unused function-typed inputs may remain opaque, just like abstract receivers;
they supply no callable-result or effect facts. Invoking an unknown callback
still rejects a required proof.

Immutable Text identities, exact literals and established `==`/`!=` facts also
participate in these proofs, including `old`, finite helpers, Text refinements
and declared generic/dyn concept summaries. Established equalities compose
transitively; known inequality propagates across equal values. Different unknown
Text values do not imply unequal contents, and inequality is not transitive.
Disjunctive facts do not establish either alternative. `std.text.length` supplies
the immutable UTF-8 byte length: literals have exact sizes, all lengths are
nonnegative, and equal Texts have equal lengths. These facts compose through
entry values, helpers and input-type constraints. Length-based refinement
weakening can remove a second boundary check; unknown strengthening still checks.
Unequal Texts may have equal lengths, and equal lengths do not identify contents.
`std.text.concat` also preserves ordered symbolic contents and adds byte lengths.
Bounded proofs support empty-string identity, reassociation, literal chunks and
substitution of established equal atoms, including pure helpers, `old` and
refinement weakening. They neither commute unknown strings nor solve arbitrary
word equations. Hypothetical concatenations must prove their size fits; eager
arguments and helper preconditions remain obligations even if the result is
unused. Only an executed concatenation supplies a successful-allocation size
bound on normal continuation. Symbolic words are limited to 256 atoms; exhausting
the limit rejects a required proof or retains a checked conversion. No runtime
proof strings or entry snapshots are allocated. Substring reasoning remains
unsupported. See the [native Text example](examples/text_contracts) and its
[concatenation contracts](examples/text_contracts/concatenation.loom).

List lengths are stateful, unlike immutable Text lengths. `std.list.length`,
List literals, `std.list.new`, indexing, `std.list.get/set` and `std.list.push`
participate in required proofs. Exact local aliases share the same extent;
append updates it and forgets other possibly overlapping extents. Indexed reads
reuse established element observations, or supply arbitrary values for unknown
elements. Writes establish their indexed value and forget possibly overlapping
observations. Successful accesses retain ordinary bounds checks and normal-return
semantics. A scalar length already read is an immutable snapshot.
New allocations are disjoint from earlier handles, but a later unknown read or
return may alias them. Copy loops can infer output-length/index equalities;
these are proposals checked on entry and every backedge, not trusted templates.

Calls with a checked extent-preserving body retain lengths. Opaque summaries
can change all reachable Lists, including through hidden dyn aliases: their
preconditions describe entry, and only their postconditions supply post-call
facts. No distinct-parameter or distinct-result non-aliasing promise is inferred.
Pure helpers and supported List length refinements compose with generic functions
and inline fields. Extent-changing loops freshen affected lengths at the inductive head;
all retained invariants still require entry/backedge proofs. See the
[List contract example](examples/list_contracts), which proves length preservation
during an actual sorting loop. The separate
[quantified example](examples/smt_contracts/quantified.loom) also proves ordering
and permutation. These proof states add no native object metadata.

Aggregate summaries compose through nested calls, field projections and
whole-value updates. Unconditional proved equalities such as
`ensures result.count == value.count` preserve that field's exact value;
unspecified fields and independent calls retain separate unknowns. A weak
summary does not inherit stronger facts from its implementation. Shared fields
can pass through an aggregate; indexed List observations follow the same alias
invalidation rules. This is not a general shared-state invariant proof. Predicate helpers still evaluate
every argument and initializer, including an unused field that could overflow.
`old` denotes immutable entry expressions: parameter paths, immutable aggregates,
arithmetic and finite pure helpers can compose (`old(value).count`,
`old(identity(value)).0`). `old(length(values))` also captures a List's entry
length, including through inline fields and finite pure helpers. These scalar
observations remain valid after append, alias mutation and nested calls; each
callee summary uses its own invocation's entry, after argument evaluation.
Operands must be entry-derived and the observed result must be immutable;
an immutable field can be selected beside an unobserved shared sibling, but
whole record/tuple snapshots containing shared mutable storage reject; logical
List snapshots are described below. Helper
arguments and predicate arithmetic still require definedness, even when a
helper ignores an argument. No entry computation or snapshot allocation enters
native code. Callback parameters and result/body-local references remain
unsupported; this is not general heap-entry reasoning.
See the [aggregate contract example](examples/aggregate_contracts/main.loom).

Required List contracts also admit `get(values, index)` and `values[index]`.
Entry predicates retain facts from their actual reads and bounds checks;
postcondition indexing must prove bounds without assuming a new check succeeds.
Literal elements, appended values, indexed writes and repeated reads establish
bounded observations (at most 64 per handle). Writes retain other indices under
inequality guards; a later branch may establish the guard. Multiple writes
conjoin their guards, using index-value snapshots, not mutable variable names.
Distinct unknown handles can still alias. Contracted
length-preserving calls can retain untouched observations using a bounded union
of their checked bodies' possible writes. Immutable parameter/inline aliases and
nested helpers compose; every branch and ordinary cleanup body contributes.
Only a proved-disjoint index on the same handle or allocation-separated storage
is framed. Mutable locals, heap-derived targets, recursion and opaque effects
supply no guessed separation. Written elements still need verified postconditions
for new facts. No-result source helpers with finite checked effects need no extra
contract to frame untouched storage. Mutating loops freshen their observed elements before induction.
Scalar values previously read remain snapshots. Contracted functions without a
return value compose through their storage postconditions. Unknown post-state
elements without an established observation or modeled content version reject.
`old(values[index])` and `old(get(values, index))` snapshot immutable scalar or
inline element values, including through finite pure helpers. Each index must
be proved in bounds on the entry paths where it is observed. Short-circuit
clauses and helper branches can supply immutable entry guards, for example
`ensures old(length(values)) == 0 || result == old(values[0])`.
Unmarked `length(values)`, `result`, and later reads/writes cannot justify an
entry access. Guarded bounds are not unconditional facts; an append does not
give a new element an entry value. Snapshotting mutable element
handles is unsupported. Entry snapshots are shared across clauses and use each
callee's invocation state, never its modified post-call storage. This permits
an in-place swap contract without a synthetic return value:

```loom
fn swap(values List[Int], first Int, second Int)
requires first >= 0 && first < std.list.length(values)
requires second >= 0 && second < std.list.length(values)
requires first != second
ensures values[first] == old(values[second])
ensures values[second] == old(values[first])
{
    let left = values[first]
    let right = values[second]
    values[first] = right
    values[second] = left
}
```

This is not quantified array reasoning. See the
[indexed List example](examples/list_elements).

For Int, Bool, Text and Float List elements and inline scalar field paths, the optional
compile-time solver also relates reads and writes
through versioned logical arrays (`select`/`store`). Contracts can prove restoration
and swapping at arbitrary parameter indices, including equal indices and the
unchanged value at any other valid index. This is an algebraic proof for every
admitted argument, not a finite enumeration of indices. The 64-observation fast
cache remains; evicting a cell does not remove its immutable version equation.
Possibly overlapping alias writes, opaque effects, loop havoc and shared
interference invalidate current versions. Old versions never become current
again merely because their facts survive. Bounds and eager fault obligations
remain independent. No runtime snapshots or solver are emitted. See the
[heap contract example](examples/smt_contracts/heap.loom).

`old(values)` also captures any List's logical entry length and outer content
version. It is not a mutable alias or a runtime copy. Pure helpers can
query it with `length`/`get`/indexing, including `old(values)[result]`: the List
comes from entry, while this index comes from return. In contrast,
`old(values[result])` is invalid because `result` is not an entry operand.
Every query must prove bounds against the captured length, never a grown current
length. Length-only queries need no solver; arbitrary content queries use the
optional array backend. Verified read-only calls retain their entry version;
possible writes never restore it from a partial frame. Each invocation owns its
snapshot. Shared workers may snapshot private or validated read-only storage, but
an unprotected shared input cannot establish one coherent entry version. No implicit locking, freezing
or ownership syntax is introduced. Int, Bool, Text and Float queries use the same
scalar theories as current arrays, including nested inline record/tuple fields
beside shared siblings. Capturing an outer header does not freeze shared children:
mutable element graphs and whole aggregates containing shared storage remain
unsupported observations. See the [entry List example](examples/smt_contracts/snapshots.loom)
and [typed entry columns](examples/list_contracts/snapshots.loom).

Bounded pure scans can express List predicates over modeled integer elements or
inline field paths without special
helper names or a new source quantifier syntax. The current fragment recognizes
a stable exclusive upper bound, an `Int` cursor incremented by one, and pure
Boolean early returns followed by a Boolean tail expression. Iterations may
contain several conditions, nested branches and local rebinding, but cannot
rebind outer state before the cursor step. Uniform false exits derive a
universal predicate; uniform true exits derive its existential dual. Mixed or
computed Boolean exits use the first exiting iteration, not any later true
witness. The tail executes only if no early return occurred.
The [search example](examples/smt_contracts/searches.loom) composes these duals,
write witnesses and loop clearing over immutable fields beside shared children.
The [branch example](examples/smt_contracts/scan_branches.loom) covers local
rebinding, ordered exits and constrained reads/replacements.
Local element
observations expand within the iteration; the tail cannot depend on the updated
cursor. A complete equality scan over `List[Int]` with a
separate zero-based accumulator derives occurrence counts. Equal lengths and
equal counts for every input element normalize to finite histogram equality;
an in-range store removes the previous occurrence and adds the replacement.
These sequence laws prove arbitrary-index swaps and permutation through calls
and loops, including duplicates and equal indices.

Loop inference proposes quantified prefix, suffix and comparison-boundary
relations from the declared postconditions. Every candidate must hold initially
and on every backedge, and survivors are rechecked after removal. The
[ordinary sorting example](examples/smt_contracts/quantified.loom) proves both
ordering and permutation without trusting the algorithm. Bound symbols remain
scoped through nested scans, helper substitution, choices and `old` snapshots.
Sufficient full-range safety is proved separately: an early return does not
establish checks for elements it never visited. Shared observations still require
private storage. Unsupported scan shapes, other mutable element domains and
exhausted inference reject required proofs; this is not general heap induction
or a termination proof. Proof operations do not enter native IR or the runtime.

A terminal local or inline field access (implicit tail or explicit `return`)
can substitute for `result` when proposing loop invariants. The sorting example
therefore returns its List with `ensures ordered(result)` and
`ensures permutation(result, old(values))`. These are hints, not an early result
binding: entry/backedge proofs and every actual return still check the original
clauses. Rebinding, early returns and cleanup cannot reuse a guessed result.
Computed/conditional tails and unavailable body-local paths supply no such hint.

The [copy and composition example](examples/smt_contracts/copies.loom) proves
element equality and permutation for a fresh output, then returns a sorted copy
using verified call summaries. Equality at every zero-based prefix index implies
histogram equality for that prefix, not handle identity or equality outside it.
Loops preserve observations of storage proved disjoint from their may-write
targets; unknown effects or rebound List receivers still havoc conservatively.
Output length may track an advancing cursor with an entry offset, checked on
every backedge, including nonempty targets and captured-length self-appends.

Direct source-call results can reuse their checked normal-return guarantees at a
constrained constructor. A private proof queue uses typed invocation snapshots
and closed immutable literals; argument effects still execute once in the
original call. Unsupported or unknown proofs retain checked `Result` construction.
List isolation is a separate requirement, not inferred from ordering alone.
The [construction example](examples/smt_contracts/constructions.loom) converts a
proved sorted copy directly, retains checks for unknown input ordering, and runs
the contracted copying/sorting functions at compile time. CTFE executes source
preconditions and bodies, not statically proved `ensures` metadata, matching native
execution without an extra return check or a runtime implementation of proof operators.
Completed sequential contract proofs are reused only within one binding/input
snapshot and exact function instance; pending promises and captured/stored
compile-time state are excluded, and shared-state proofs run independently.

Self-recursive and mutually recursive body calls can compose declared
normal-return contracts. The private typed helper closure checks every source
member against the group's guarantees before publishing any result; a failed
member rejects the whole group, including after cached source edits. This is
partial correctness by induction on finite call depth, not a termination or
fault-freedom promise. Contract predicates still expand actual finite pure
bodies: a recursive predicate cannot become an axiom from its own `ensures`.
See the [recursive contract example](examples/recursive_contracts/main.loom).

Length-preserving loops can infer bounds/equalities for observed Int, Bool and
Text elements, and scalar field relations in immutable inline elements. Source
pre/postconditions may propose storage conservation laws; they are not assumed
merely because they are declared. Every candidate must hold at the loop entry
and all backedges. Removing one candidate rechecks its dependents, early returns
and exits. This covers incrementing a nonnegative element and transferring a
bounded amount between two elements while conserving their sum. Ordinary block
cleanup runs before `continue`/`break` checks and preserves return snapshots.

These observations refer to fixed entry indices, not an entire array. Extent-
changing loops forget affected contents; opaque effects forget all possibly
reachable contents. Mutable element handles
are not snapshotted; a possibly overlapping write removes evidence. The existing
64-candidate budget bounds inference, and no loop termination claim is inferred.
See [loop examples](examples/list_elements/loops.loom).

Required postconditions reuse the integer difference propagation used at
refinement boundaries: `requires value > lower && lower >= 0` can establish
`ensures result > 0` for a body returning `value`. Inline record/tuple leaves and
fresh results with verified callee bounds retain distinct symbolic identities,
including through scalar assignment and snapshots. Short-circuit postconditions
use their branch guards without assuming unevaluated arithmetic succeeded.
Propagation is bounded to 256 facts and 32 numeric identities at a boundary;
unknown required proofs still block compilation. After direct facts fail, a
bounded shortest-path query also proves relative chains such as `a > b` and
`b > c` implying `a > c`, or equality from a zero-weight cycle. Negative cycles
establish inconsistent premises. The graph uses mathematical arithmetic and
unit-coefficient differences; it does not assume hypothetical source arithmetic
is defined or solve general nonlinear constraints. It adds no runtime mechanism.
See the [relational contract example](examples/relational_contracts).

`while` bodies with Int/Bool/List locals or supported inline records/tuples can
prove normal-return contracts through inferred invariants, without new annotations.
The prover freshens scalar leaves and List handles of written locals, then
proposes per-path entry/range bounds, List length bounds and weakened relational
guards. Written Int guard cursors also propose entry-difference relations with
other written Int locals, after existing storage/quantifier proposals and within
the same candidate budget. Constant affine translations additionally suggest
weighted pair relations, such as `total - 2 * cursor` or `total + remaining`.
Finite pure helpers and exact scalar/inline-field result summaries can supply
the same translation coefficients, including declared concept guarantees.
Projection reuses the ordinary result template without requiring facts about
unobserved shared siblings. This optional expansion has a separate 256-step
budget; an opaque result or exhausted suggestion supplies no assumed value.
These mathematical candidates connect separately updated counters and symbolic
offsets; translation syntax is not evidence and source arithmetic still needs
its own definedness proof. Candidate generation remains guard-anchored and
pairwise, not general affine invariant synthesis. Rebinding a List preserves no
handle identity. If a loop can both rebind handles and resize storage, all tracked
extents are freshened before induction;
its initial binding cannot identify every later resize target. Whole-value
reconstruction does not retain stale sibling fields. Other unsupported leaves
are not admitted as written aggregate locals. Each candidate
must hold on entry and on every symbolic backedge; removing a candidate rechecks
all survivors. Zero-iteration exits and early returns remain separate proof
obligations. Branches, nested loops and the existing direct-call proof rules
are supported. `break` records the state at that exit; `continue` contributes a
backedge that must preserve every retained invariant. Nested jumps target the
nearest loop. Scalar lexical cleanup executes before jumps in its checked order,
preserving evaluated return snapshots. Guard calls execute with fresh result
identities using verified summaries or finite pure expansion; their syntax is
not reused as a stable invariant term. Optional pure expansion can suggest bounds,
but contributes no assumed facts and every candidate still needs induction.
Resource cleanup and unmodeled heap operations inside a
required loop proof still reject; the List extent operations above are supported. The bounded
inference neither unrolls a sample of iterations nor adds runtime invariant
checks. See the [loop contract example](examples/loop_contracts), including
[nested aggregate state](examples/loop_contracts/aggregates.loom).

Immutable input refinements contribute their supported predicates to required
proofs without a duplicate `requires` or runtime check:

```loom
type Positive = Int where self > 0

fn amount(value Positive) Int
ensures result > 0
{
    value
}
```

The same applies to inline
record invariants and refined leaves inside records/tuples. Facts belong to the
actual symbolic value, not the spelling or slot of `self` in the template.
Successful typed List reads, checked conversions, verified call returns and
loop-local replacement values retain the same supported invariants. Thus a
`List[Positive]` read can prove a positive result without a wrapper or duplicate
assertion. This extends to immutable inline fields and Text refinements. `old`
and hypothetical current reads retain their access-validity premises; an inactive
branch cannot introduce unconditional facts. The
[typed worker example](examples/workers/typed.loom) exercises these guarantees
under mutation without assuming equal old/current values.
Bounded List predicates also establish sequential entry observations, including
nested fields and `old(values[0])`. They use the same guarded-read machinery as
preconditions, not duplicate runtime checks. Unknown alias writes invalidate
these observations; length-only facts imply no element contents. Shared workers
require separately validated storage guarantees.
Int/Bool predicates use the existing bounded
fragment. If direct facts are insufficient, acyclic pure predicate helpers expand
in a private checked closure, retaining their guarded preconditions and successful
checked calculations. This does not add runtime calls or change construction
checks. Unsupported conjuncts and general helper loops/recursion supply no
evidence. See the [input invariant example](examples/invariant_contracts).

The current proof fragment supports scalar linear arithmetic, comparisons,
Boolean facts, local assignments, branches/returns, and the scalar loops above. It reasons from
preconditions and successful checked operations. A source-written `assert`
provides a fact only after that assertion succeeds; the compiler never inserts
an assertion to rescue a failed postcondition proof. Postcondition arithmetic
must itself be defined within `Int` bounds.

Pure-helper expansion supports the bounded scans above, but still excludes
general loops, shared-storage mutation, cleanup and returns inside operands;
verified callee summaries use the separate rules above.
Recursive predicate expansion, dynamic calls without a usable declared contract
and indirect calls remain outside this proof fragment. IEEE Float proofs reuse
exact comparisons over immutable
values, including constrained inputs/elements, verified returns and guarded
entry observations. Constant expressions use binary64 evaluation; symbolic
arithmetic retains its operand order and structure. A bounded comparison graph
composes strict/non-strict order, constant range endpoints and numeric equality.
True comparisons establish non-NaN endpoints, allowing reflexivity and reversal
of negated ordered comparisons only for those endpoints. Contradictory order
paths exclude unreachable branches, including short-circuit return paths.
Numeric equality does not identify computations: signed zeros compare equal but
have different reciprocals. No unguarded cancellation, reassociation or real-number
arithmetic is inferred. See the [range/clamp example](examples/floats/order.loom).
Float locals and literals may participate in proved loops, including variadic
induction; assigned values are freshened at loop heads, not assumed to retain
entry comparisons. Established Float entry bounds also propose scalar and inline
field invariants. Every candidate needs an entry proof and every symbolic
backedge proof; supported unresolved Float comparisons use the same IEEE SMT
backend. Removed candidates trigger rechecking, including early exits. Unknown
or failed solver work establishes nothing. See the
[Float loop example](examples/loop_contracts/floats.loom). This is bounded order
inference, not general Float arithmetic invariant synthesis.
Unresolved supported Float obligations use SMT-LIB's binary64 FloatingPoint
theory, with `fp.eq`/ordered comparisons, negation and RNE-rounded `+`, `-`, `*`
and `/` in source order. Literal conversion preserves subnormals, infinities,
NaN and zero's sign. This proves magnitude and bounded scaling contracts without
assuming finiteness, reassociation or real-number identities. Symbolic `%`
uses the magnitude correction in [C23 F.10.7.1](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n3220.pdf),
not raw [SMT `fp.rem`](https://smt-lib.org/theories-FloatingPoint.shtml):
[LLVM `frem`](https://releases.llvm.org/22.1.0/docs/LangRef.html#frem-instruction)
truncates the quotient, whereas `fp.rem` rounds it to nearest. The encoding
preserves signed zero, NaN, infinite divisors and exact subnormal remainders.
IEEE classification rules avoid unnecessary circuits; a single explicit
alternative may be split before branch-local equalities are substituted.
Every branch remains a required proof under the original solver time budget.
See the [Float trial](examples/floats/contracts.loom),
[arithmetic contracts](examples/smt_contracts/floats.loom) and
[remainder contracts](examples/smt_contracts/float_remainders.loom). Source `std.float.abs`
uses ordinary branches/arithmetic, preserves NaN and maps both zero signs to
positive zero. Its finite pure body supports checked caller contracts, which
then compose at refinement boundaries.
Solver work is bounded; exhaustion is a diagnostic,
not permission to trust an obligation. These are normal-return guarantees, not
proofs of termination or absence of runtime faults.

## Compile-time value parameters

Mark a named parameter `comptime` when its value must be known during checking:

```loom
fn increment(value Int) Int { value + 1 }
fn repeat[T](value T, comptime action fn(T) T, comptime count Int) T {
    comptime if count <= 0 { value } else {
        repeat(action(value), action, count - 1)
    }
}
fn main() { assert repeat(39, increment, 3) == 42 }
```

Static parameters accept `Int`, `Bool`, `Float`, `Text`, immutable stored records,
tuples, enums and constrained values, plus function values as whole parameters.
Inferred type parameters and associated types can specialize to these shapes:

```loom
fn constant[T](comptime value T) T { value }
fn main() {
    assert constant(42) == 42
    assert constant("text") == "text"
}
```

The same forms work on concrete and dynamic methods. Abstract declaration
checking keeps `T` abstract even when a caller supplied a known value;
requirements must still be declared or selected with `comptime if T implements C`.
Unsupported concrete parameter shapes reject
at specialization, not during native emission. See the
[generic static example](examples/comptime_parameters/generic.loom).

Aggregate fields must recursively use immutable stored values; every enum
variant is checked, not only the selected one. List, Bytes, Task, dyn values and
stored callbacks are not static aggregate fields. MustScope obligations still
apply before parameter erasure. Ordinary copies remain runtime values and keep
their normal mutability; this adds no ownership syntax or deep-copy semantics.
Structural keys include nominal types, enum tags, ordered fields and framed Text,
so equal configurations reuse one instance independently of initializer order.
Constrained construction and widening keep their existing checks and proofs.
Static tuple/record iteration preserves field identities through nested maps,
shadowing and closures. See the
[aggregate parameter example](examples/comptime_parameters/aggregates.loom).

Float specialization uses the same round-tripping numeric encoding as ordinary
compile-time evaluation: equivalent finite values share an instance, positive
and negative zero remain distinct, and NaNs use the existing canonical encoding.
This is an instance key, not a change to IEEE value equality; NaN still differs
from itself. Subnormals and infinities are preserved. Float parameters also work
through static packs, closures and dynamic method slots, without runtime
parameter storage or a new proof rule. See the
[Float static example](examples/comptime_parameters/floats.loom).

Literals, pure computed expressions and forwarded static parameters specialize
the declaration; static values participate in instance and computation keys.
Ordinary parameters and captured callback environments remain in the native ABI,
with their original relative evaluation order. A runtime value is an error, not an implicit runtime overload
or fallback. Ordinary surrounding `let` bindings are not automatically promoted
to static bindings.

Function parameters accept named references (including contextually selected
overloads and generics), forwarded static references, and pure computed
selectors. Instance keys contain the source function and its concrete type
arguments. Non-Task calls to static targets are direct before LLVM optimization;
Task-returning callbacks retain their typed creation-location adapter.
Naming an effectful function does not execute it, but calling it during
compile-time evaluation still rejects. Ordinary runtime callback parameters
remain available when the target is not known during checking.

Captured callbacks keep that static identity but pass their environment as
ordinary managed data. The argument's pure construction materializes once per
source call; forwarding passes its current shared state, and a separate
construction starts fresh. Captured contents do not duplicate native instances
with the same target and layout. A nested `comptime` block cannot read this live
environment; the whole call can instead execute at compile time. Merely
constructing an effectful or async callback is allowed; actually invoking it in
the evaluator still rejects I/O and Tasks. Indirect purity checking considers
all discovered targets of the checked shape, not only the observed branch.
See the [captured callback example](examples/comptime_closures/README.md).

Selected static branches are checked with abstract type arguments and declared
requirements before concrete emission. A Boolean switch does not supply missing
generic capability evidence. Required postconditions still have to pass abstract
declaration checking, even on unused functions; unsupported parameter-dependent
proofs reject. Specialization and pure evaluation remain bounded.

The [static-parameter example](examples/comptime_parameters/main.loom) covers
generic recursion, pure argument computation, static Text, shadowing, returned
ordinary and static callbacks, shared results and runtime argument order. Concept
and implementation methods support the same parameter forms, including dynamic
calls; intrinsics do not. Taking a reference to a declaration with static
parameters still rejects until explicit partial
specialization can supply a complete function identity. Static value packs use
the [variadic form](#variadic-functions). [Type-valued computation](#first-class-types)
uses explicit compile-time staging rather than runtime type tags.

## Compile-time execution

`comptime { ... }` evaluates a complete expression during checking. In contrast,
`comptime if` evaluates only its condition and checks the selected body as normal
code, which may use runtime parameters:

```loom
fn adjusted[T](value T) Int {
    comptime if T == Int { value + 1 } else { 0 }
}

fn main() {
    let count = comptime {
        var n = 0
        while n < 3 { n = n + 1 }
        n
    }
    assert adjusted(count) == 4
    assert adjusted("text") == 0
}
```

The [comptime example](examples/comptime/main.loom) also exercises ordinary pure
function calls, recursion, local mutation, records/enums, and fresh list aliases.
Results support `Bool`, `Int`, `Float`, `Text`, tuples, records/enums, shared lists, and byte
buffers. Refined results retain their underlying representation and validated
nominal type; nested shared containers keep the same graph identities, including
constrained Lists captured by returned callbacks. Original construction, escapes
and mutations still pass the ordinary checks before evaluation; restoring the
computed value is not a second source conversion. See the
[refined-result example](examples/comptime/refined.loom).
Scalar values become constants; containers are allocated and populated
whenever the expression runs. Each runtime evaluation gets a fresh graph, while
aliases and cycles inside that graph are preserved. Mutating one invocation's
result cannot affect the next; there is no hidden mutable global or package
initialization. The [shared-result example](examples/comptime/shared.loom)
exercises these distinctions.

Explicit blocks cannot read or write surrounding runtime locals, or return from
the enclosing function, including through `?`; called functions may return
normally. Arbitrary I/O and ambient external inputs are disallowed. The explicit
build-input API below embeds tracked snapshots before evaluation. Calls and loops are
bounded by work, depth, and allocation limits; faults or exhausted limits are
diagnostics, never a fallback to runtime execution.
Result expansion is bounded too. Successful pure computations may be reused
within one check after type/capture validation, but each use still reconstructs
fresh runtime containers. This is not a persistent or incremental build cache.

### Explicit build options

```loom
import std.build.option

fn channel() Text {
    option("app.channel", "development")
}
```

Run `loom build --build-option app.channel=preview`. Repeat the flag for other
names; duplicates reject instead of using argument order as precedence. Names
start with an ASCII letter or `_`, followed by letters, digits, `_`, `-` or `.`.
Values are Text, may be empty, and may contain `=`. Options apply to the complete
selected application/dependency closure; qualified names avoid accidental collisions.

`option(comptime name Text) Option[Text]` distinguishes absence from an empty
value. `option(comptime name Text, fallback Text) Text` supplies a default through
ordinary source code; like other function arguments, the fallback is eager.
Options work in `comptime if`, pure compile-time evaluation and ordinary calls.
The compiler substitutes constants; there is no runtime environment lookup.
Environment-derived values must be passed explicitly by the calling shell/build
step. Values can enter artifacts and are **not a secret channel**.

`check`, `build`, `test`, `run`, and `emit-checked` accept the same options.
Frontend-cache keys include the canonical option map. V3 build receipts include
names and value digests. Public checking/analysis can supply `BuildInputs.options`
without filesystem access; analysis snapshots and freshness checks include it.
Editor requests use workspace `loom.buildOptions`. Target metadata is separate,
not inferred from option names. See the
[runnable example](examples/build_options/main.loom).

### Target properties

`std.build.target(comptime property Text) Text` reads `os` (`macos`, `linux`,
`windows` on the supported hosts), `arch` (`aarch64` or `x86_64`), `pointer_width`
(`"64"`), or `endian` (`"little"`). These describe the actual backend target,
not the frontend's host or the width of Loom `Int`. Unknown names reject.

```loom
import std.build.target

fn line_end() Text {
    comptime if target("os") == "windows" {
        "\r\n"
    } else {
        "\n"
    }
}
```

Properties are fixed while checking, work in compile-time evaluation and become
ordinary Text constants. The driver reads its selected native backend lazily,
once per checking context, retaining all four properties as one coherent
snapshot. Importing the declaration or skipping a query in
an unselected `comptime if` branch needs no backend. This is target introspection,
not cross-compilation support or runtime platform detection.

The requested target snapshot enters the checked artifact; emission rejects a mismatched
target, including when consuming saved `emit-checked` output. Frontend-cache hits
revalidate the properties before reuse, and the receipt's checked-input digest
binds them. Optimization and CPU tuning do not change these four properties.
Public analysis accepts explicit `BuildInputs.targets` snapshots or `read_target`
callbacks with an explicit `target_context`, without a subprocess; freshness
includes the snapshot. A supplied snapshot must contain every requested property.
Editor queries use the compiler's discovered backend or `loom.nativeTool`, matching
CLI `--native-tool`. See the [native example](examples/build_target/main.loom).

### Tracked build inputs

```loom
import std.build.input_file

fn banner() Text {
    input_file("assets/banner.txt")
}
```

`input_file(comptime path Text) Text` embeds a UTF-8 file as a constant. Paths
resolve relative to the package containing the call, including calls in imported
helpers, and must stay within that package's module after symlink resolution.
No working-directory lookup or runtime file read is generated. A missing,
non-file or invalid UTF-8 input is a checking error. Use explicit runtime file
APIs when the program must read current contents instead.

`input_bytes(comptime path Text) Bytes` embeds raw binary data instead. Every
evaluation creates a fresh mutable buffer; repeated calls do not share mutations
or modify the saved build input. Text and binary requests for the same path share
one tracked byte snapshot. Binary literals, including compile-time-computed
buffers, lower to a static data block and a bulk copy, not one append per byte.
Sharing within a compile-time result graph is preserved. Source `std.encoding.hex`
provides `encode(Bytes) Text` and strict `decode(Text) Result[Bytes, DecodeError]`.

Dependencies are discovered automatically during checking; repeated requests use
one snapshot. Ordinary checked bodies can request inputs even when not runtime
reachable; unselected `comptime if` branches do not. A `comptime` path parameter
can defer a helper's request until specialization. The frontend cache rechecks
resolution and bytes, and build receipts record request identity and content
digests, never file contents. Output/IR/receipt paths cannot replace a selected
input. Embedded data is visible in the executable: do not use this for secrets.

Public `InputFile.bytes` snapshots carry raw bytes; analysis detaches them before
checking. See [the runnable example](examples/build_inputs/main.loom). Target properties use the
separate API above. Network access and commands are not compile-time operations.

Every branch must parse, but unselected `comptime if` branches impose no type or
call requirements. Type guards compare unshadowed types with `==` or `!=`,
including generic/nested forms such as `T == List[Int]` and
`List[Box[T]] == List[Box[Int]]`; see the
[composite-guard example](examples/comptime/composite.loom). These guards do not
provide general type-valued expressions. `!`, `&&`, and `||` combine type guards,
`implements` queries, static Bool parameters, and ordinary pure Bool computations.
For example, `comptime if T implements Display && (T == Int || T == Bool)`
selects a body with local Display evidence; the
[conditional-label example](examples/comptime/conditions.loom) needs no public
Display bound.

Conditions short-circuit left to right at compile time: a skipped operand still
parses but is not instantiated. Established concept evidence follows the actual
condition path into subsequent operands and the selected body, including an
`else` reached through negation. An unresolved left operand waits for concrete
instantiation instead of inspecting the right operand or applying Boolean
identities that could hide a fault. Unresolved choices require a contextual
result type; required proofs cannot assume their result. Code outside the choice
is still checked. Ordinary runtime Boolean expressions keep their existing
type and purity checks.

The Loom-written evaluator consumes the same checked model as native lowering;
constraint folding and pure-predicate validation use this engine too.
Evaluation is not proof: helper calls in function contracts undergo symbolic
expansion, and declared postconditions still require the prover.
Float compile-time operations and numeric codecs use the same IEEE behavior as
native code, including NaN, infinity, signed zero and subnormals. Float/refined
results may appear inside shared aggregates. The bounded Float proof rules above
preserve IEEE semantics; successful evaluation is not a general algebraic proof.

### Typed expression macros

`generator!(values...)` runs an ordinary pure
`fn(List[std.reflect.Schema]) Text` (or `std.loom.ast.Node` result) at compile
time, with one descriptor per inferred argument type. Text is parsed as one
expression; structured AST output must round-trip through that grammar, then is
checked in the generator's package. `$0`, `$1`, ... refer to arguments evaluated
exactly once, left to right; other names cannot capture caller locals. Generators
need no separate package. Normal contracts, visibility, cleanup and Task rules
apply to expanded code. The generator itself adds no runtime call edge.

See the runnable [typed macro example](examples/typed_macros), including generic
selection, heterogeneous inputs, closures, cleanup and required proofs. `loom fmt`
preserves macro punctuation; editor hover shows the expanded result type and
definition navigation targets the source generator. `std.loom.syntax` also
emits validated file/declaration/type/pattern fragments from public AST data.
The [declaration tool](examples/ast_generation) generates a normally checked
package on disk. To generate declarations in the current compilation instead,
use a top-level block:

```loom
comptime {
    """
    pub fn answer() Int {
        42
    }
    """
}
```

The block returns Text or a public AST `File`/declaration through ordinary pure
Loom calls. All stages see the original source, not other stages' output; compose
generator functions explicitly. Generated names belong to the invoking package
and retain explicit `pub`/`test` rules. Output cannot introduce imports or another
declaration stage. Normal binding, type, resource and required contract checks run
on the result, without writing source files. This is explicit declaration
generation, not the definition-site hygiene of expression macros.

The [in-compilation example](examples/declaration_generation) covers generated
records, refinements, implementations, imported functions and isolated tests.
Editor diagnostics and navigation point to the generating block; generated names
are not text-renamed. Tracked build inputs also invalidate generated analyses.
`std.loom.analysis.analyze` and `check_project_cached` include expansion; low-level
clients call `expand_project` before `bind`/`check`. Analysis retains the raw
`source` snapshot separately from expanded bindings for freshness checks.

### Compile-time type values

`type` is a compile-time-only value type. It does not reserve the ordinary name
`Type`, expose compiler indices, or add runtime type discovery:

```loom
fn choose(flag Bool, first type, second type) type {
    if flag { first } else { second }
}

fn main() {
    let Number = comptime { choose(true, Int, Float) }
    let answer Number = 42
    assert answer == 42
}
```

Pure functions can pass, return, compare and store types in compile-time data.
`std.meta.of[(Int, Text)]()` and `of[fn(Int) Bool]()` provide compound types.
Known immutable type bindings supply local annotations, generic arguments and
visible record/enum/refinement constructors and record/enum patterns. A selected
pattern must match the exact instantiated type, not just its nominal declaration.
A `comptime { ... }` block can also
supply a type directly in a local annotation or explicit generic argument.
Nominal identity, visibility, bounds and construction checks are unchanged.
Type-bearing data cannot escape into runtime locals, parameters or results;
source-public type-producing functions have no native export.

Type construction follows explicit staging: `std.meta.list(comptime element type)`
requires its argument to be known at that call. A type-valued evaluator local is
not an implicit generic parameter. Declaration signatures still use `[T]`, not
value-dependent return-type inference. See the [type-value example](examples/type_values).

`std.meta.tuple(elements List[type])` and
`std.meta.function(parameters List[type], output Option[type])` construct
structural types inside compile-time execution. Lists can be computed with pure
loops and local mutation; their selected shape becomes an ordinary static type,
not a runtime-sized tuple or parameter pack. Construction shares the checker's
type interner, so a computed type equals the same directly written type.
An empty list produces an empty tuple, distinct from an omitted result;
`Option.None` selects an omitted function result. Nested types, nominal identity,
constraints, visibility and runtime resource obligations keep their ordinary
rules. Type lists and constructor calls cannot escape into native values.
See [construction.loom](examples/type_values/construction.loom).

Pure construction can also preserve a generic parameter's symbolic identity:

```loom
import std.meta.tuple

pub fn tagged[T](value T) (T, Text) {
    let Tagged = comptime { tuple([T, Text]) }
    let tagged Tagged = (value, "tagged")
    tagged
}
```

The public signature still declares its result and required concepts. A computed
alias grants no additional methods, constraints or resource permissions. Type
comparison composes over structure: `(T, Text)` differs from `(Int, Bool)`, but
`T == Int` remains unknown until specialization. Unknown comparisons cannot select
a branch or discharge a proof. Shape-changing computations that cannot produce
an abstract type still reject rather than infer a public requirement from one
concrete call. See [symbolic.loom](examples/type_values/symbolic.loom).

Declared associated types are type values too: `S.Item[T]`, qualified
`S.Concept.Item[T]`, and `Self.Item[T]` in methods. An immutable selected receiver
type supports the same projections. Resolution retains the ordinary declared
bounds, concept disambiguation and lexical visibility; a runtime value that
shadows the receiver name is not a type. Enum members such as `Selected.None`
remain value constructors. See [projections.loom](examples/type_values/projections.loom).

### Type reflection

`std.reflect.describe[T]()` resolves the declared type at its lexical call site
into ordinary source `Schema` data. Generic helpers can query an inferred `T`:

```loom
import std.reflect.describe
import std.reflect.Kind

fn is_integer[T](value T) Bool {
    discard value
    comptime if describe[T]().types[0].kind == Kind.Int {
        true
    } else {
        false
    }
}
```

`Schema.root` is zero; `types` is a finite graph. Field, variant-payload and
argument indices refer only to that graph, including recursive and repeated
types. Nominal `name` labels omit source paths and are not identities.
`Type.arguments` contains generic arguments or tuple elements; functions use
parameter types followed by the result, Lists/Tasks their element/result, and
refinements their base type. `NoResult` describes an omitted function result;
it does not introduce a source `Unit` type. Records expose ordered `fields`,
enums ordered `variants`, and `Dynamic` named associated bindings in `fields`.

Private types outside the call's package become `Opaque` leaves with empty
names and no exposed structure. A generic library helper retains its own
lexical visibility, not its caller's. Dynamic reflection describes the declared
interface, never the erased concrete receiver. Methods and values are not
enumerated, and descriptors do not establish compatibility proofs.

The separate [`std.reflect.predicates.describe[T]()`](std/reflect/predicates/README.md)
returns an optional direct refinement predicate: owner label, canonical source
and public AST with spans into that source. It follows the same lexical visibility;
ordinary and inaccessible types return `None`. Helper names retain their original
scope, not new access permissions. Helper bodies and inherited predicates are not
expanded, and syntax data is not proof evidence. Generated refinements use the
same path. Mutating a returned AST cannot change a type's constraint.

Descriptors have fresh mutable Lists on each runtime evaluation; ordinary
copies share those Lists. `comptime` can consume or return them, and a fully
compile-time query leaves no descriptor allocation in native code. There is no
runtime registry or new reflection ABI. Call `describe` directly; a normal
source wrapper can serve as a function value. Descriptors are structural metadata,
not first-class type values. Static type iteration and `from_fields` supply a
separate, checked path for structured code generation without interpreting
descriptor indices as types. See the
[reflection example](examples/reflection/main.loom).

## Lexical cleanup

`defer { ... }` registers a synchronous block in its containing lexical scope,
including `if`/`else`, match arms and each loop iteration. Registered blocks run
once in reverse order on normal completion, `return`, `Result?` propagation,
`break`, or `continue`, and before termination on a synchronous language
`RuntimeFault`.
Bindings are resolved at registration, but their values are read at cleanup:

```loom
var value = 1
{
    defer { value = value + 1 }
    value = 4
}
assert value == 5
```

A tail or returned value is saved before cleanup, including managed aggregates.
Fresh MustScope results remain provisionally protected until outgoing cleanup
succeeds; a cleanup fault disposes the untransferred result and its resource members.
Cleanup must have no value result; ordinary `discard` remains explicit. Its
body cannot contain `return`, `?`, `scoped`, or another `defer`, even in an unselected
compile-time branch. Loop control is allowed only for loops inside the cleanup;
it cannot leave the cleanup. Called functions, including closures defined inside
the cleanup, have their own return, cleanup and loop scopes.
Pure cleanup also executes during compile-time evaluation. Native lowering uses
direct callbacks and stack registrations, not a general runtime executor. Callbacks
read and update the owner's local storage, including moving-GC roots. Programs
with reachable cleanup route faults through a small LIFO drain, including faults
in called functions without local `defer` blocks. Cleanup-free scalar executables
retain their runtime-free path.
Required proofs still inspect the lowered function; unsupported proofs reject.

The [cleanup example](examples/cleanup/main.loom) exercises these exits and GC
snapshots. Fault draining preserves the first diagnostic and runs remaining
callbacks even if a cleanup faults. Ordinary source programs still terminate;
the native resume catcher used by Tasks is not source-level recovery. OOM, internal
runtime corruption, external process signals,
and explicit process termination do not guarantee cleanup. Task cancellation
drains queued or suspended descendants before parent cleanup. Async captures
live in moving frame fields, not stack registrations retained across await.

`scoped name [Type] = initializer` uses the same block cleanup mechanism. It
evaluates its initializer once and registers the statically selected
`std.resource.Dispose.dispose(self Self)` only after success. Cleanup returns
no value. The binding cannot be reassigned, copied, discarded, returned, or
manually disposed; checked receiver methods may still mutate ordinary shared
data. Implementing Dispose alone does not change ordinary unscoped sharing.

```loom
import std.resource.Dispose
import std.resource.MustScope
import std.list.push

record Ticket { trace List[Int] }
impl Dispose for Ticket {
    fn dispose(self Ticket) { push(self.trace, 1) }
}
impl MustScope for Ticket {}

fn main() {
    let trace List[Int] = []
    {
        scoped ticket = Ticket { trace = trace }
    }
    assert trace[0] == 1
}
```

The empty `std.resource.MustScope` marker requires immediate scoped handling,
not ordinary bindings, parameters, discard, or dynamic erasure. A statically
checked factory may return a fresh resource directly; a fresh Result/Option can
transfer its single resource through `?` or a match payload immediately bound
with `scoped`. Already shared bindings are not silently consumed. Checks follow
receiver calls and also validate unused concrete resource functions without
emitting them. No ownership, borrowing, or lifetime syntax is introduced.

Runtime function values and dynamic factory methods may also return MustScope
resources, including the supported Result/Option transfers. Each selected source
implementation must satisfy the same fresh-return obligation; indirect calls do
not bypass body checking. For example, with `Ticket` above:

```loom
fn create(trace List[Int]) Ticket { Ticket { trace = trace } }
fn use_factory(make fn(List[Int]) Ticket, trace List[Int]) {
    scoped ticket = make(trace)
}
```

This guarantee follows MustScope, not Dispose alone. A function returning an
ordinary Dispose-only value may return a shared alias, so its function type alone
cannot justify a scoped initializer. Direct calls still use checked body evidence.

Nested record and tuple resources receive pending cleanup during construction.
Generic parameters and associated fields can use `Dispose + MustScope` bounds
for the same transfer. Concrete specialization registers every nested resource,
not just the abstract field's outer Dispose method; completed fields still drain
if a later initializer or a cleanup faults. See the
[generic cleanup tests](examples/cleanup/generic_resource_test.loom).
Later fields may create closures or use field-local loops: a closure's return
and a local loop's break/continue do not escape aggregate construction. Discarded
`comptime if` branches do not participate in this check. Actual
enclosing returns, propagation and loop exits still reject while a resource
field is pending. Pending fields remain protected across awaits and are drained
on cancellation; NoSuspend fields still forbid suspension. Tuple literals and
`comptime map` use the same checked construction path. Source `std.resource`
provides structural tuple and Option/Result container hooks, without making
ordinary Option/Result data depend on resources.
Enum payloads use the same pending transfer and LIFO cleanup, selecting only the
active variant. An enum used with `scoped` must implement Dispose; its MustScope
payloads are cleaned automatically after the enum's own Dispose method. Matching
that scoped enum or a borrowed receiver borrows payloads, including nested and
guarded patterns. It does not permit copying, returning, manually disposing, or
putting those payloads into another `scoped` binding. See the
[enum cleanup tests](examples/cleanup/resource_enum_test.loom).
Fresh, unscoped match payloads may immediately transfer into new record, tuple,
enum or List constructors. Existing payloads receive pending protection before
other field expressions run; those expressions retain their source order.
Failures and cancellation drain the pending values, while successful construction
transfers them to the new aggregate. Borrowed/scoped values, duplicate transfers
and abandoned payloads still reject. NoSuspend payloads cannot cross an await.
See the [reconstruction tests](examples/cleanup/resource_reconstruction_test.loom).
Resource Lists support fresh literals and dynamically sized construction:

```loom
import std.resource.generate

fn use_tickets(count Int, trace List[Int]) {
    scoped tickets = generate(count, fn(index Int) Ticket {
        create(trace)
    })
}
```

`generate` requires a nonnegative count and a fresh `Dispose + MustScope` result
from each factory call. Construction preallocates the outer List; if a factory
fails, already-created elements still close. Scope exit cleans elements in reverse
order, continuing through nested cleanup faults. Read-only calls and indexing
borrow the scoped container; they cannot copy, return, capture, re-scope, mutate,
or manually dispose resource elements. MustScope parameters are checked borrows,
including indirect calls; callers must scope fresh arguments first. Async borrows
require a directly awaited call, so the owner outlives its child Tasks and their
cleanup. The borrowing Task cannot be saved, forwarded or returned. No borrowing
syntax is required. See the
[List cleanup tests](examples/cleanup/resource_list_test.loom).
Inline scoped records, tuples and enums may also contain Task fields. The owning
scope must explicitly consume those fields; resource parameters borrow them and
cannot await or transfer them. Task-free fields remain readable through ordinary
helpers, including cleanup. Such a borrow does not grant a synchronous helper
authority to create unrelated Tasks. Cleanup neither waits nor cancels implicitly;
fault/cancellation drain child Tasks before disposing their enclosing resources.
Read-only matches also retain the scoped enum's Task obligations, before or after
Task consumption. A consuming match still checks every transfer and continuing
path. Read-only payload bindings can leave their block through loop control.
`NoSuspend` keeps its stronger rule. See the
[mixed scope tests](examples/cleanup/resource_task_scope_test.loom).
Resource Lists containing live Task elements and Dispose-only mixed aggregates
remain conservative; this is not general Task-bearing resource mutation support.
The four-argument `generate(count, state, next, finish)` overload also accepts
ordinary elements and an explicit cursor. `next(index, state)` returns
`(element, next_state)`; `finish(state)` consumes the final state before the List
transfers. State may carry one-shot Tasks but cannot copy a scoped resource.
Resource elements remain guarded if a later factory call or `finish` faults.
This is synchronous construction, not a Task or executor per element. See the
[stateful construction tests](examples/cleanup/resource_state_test.loom).
Recursive record/enum layouts through Lists also clean up through finite typed
helpers. Recursive factories retain the fresh-return requirement; read-only
recursive traversal borrows the tree. Copying or mutating scoped resource edges
cannot manufacture shared ownership or cycles. See the
[resource tree tests](examples/cleanup/resource_tree_test.loom).
Lexical cleanup across suspension is
supported as described in [Source Tasks](#source-tasks).

`break` exits the nearest enclosing `while` body; `continue` reevaluates that
loop's condition. Both run the defers of scopes they leave, but not defers
outside the loop. A loop condition is outside its own body's control scope.
Labels and values on loop-control statements are not supported. A `comptime`
block evaluates its own loops and cannot jump into a runtime loop; a selected
`comptime if` branch is ordinary code at its insertion point. Loop execution is
supported at compile time. Required proofs support inferred scalar invariants,
including break/continue paths, but not general loop contracts.
The [loops example](examples/loops/main.loom) includes same-package unit tests.

## Scoped mutexes

`std.sync.mutex` exports `Mutex`, `Guard`, `new()` and `lock(mutex)`. Mutex copies
share one lock. Acquire it with `scoped`; the guard implements `MustScope` and
`NoSuspend` and releases at the containing block exit, including returns and
language faults. Guards cannot be copied, reconstructed from extracted fields,
discarded or held across `.await`. No ownership or borrow syntax is introduced.

```loom
import std.sync.mutex.new
import std.sync.mutex.lock

fn main() {
    let mutex = new()
    if true {
        scoped guard = lock(mutex)
    }
    scoped next = lock(mutex)
}
```

Acquisition is synchronous and non-reentrant; locking the same mutex again on
its owning thread faults instead of deadlocking. Fault cleanup releases the
guard without poisoning the mutex. There is no fairness promise. Every relevant
access must follow the same locking policy; holding a guard alone does not prove
that other aliases preserve an invariant. See the [example](examples/mutex)
and `loom test compiler/std/sync/mutex`. This API does not change ordinary Task
scheduling; explicit [workers](#shared-workers) can use it for compound updates.

## Shared workers

`std.task.worker.run(work)` creates a hot `Task[T]` for a synchronous `fn() T`
callback. Captures retain ordinary sharing, including mutable captured bindings;
there is no implicit copy or ownership syntax. Ordinary async functions still
run cooperatively. Workers run on a separate bounded CPU pool (up to four native
threads), using the same moving heap and existing Task completion notifications.

```loom
import std.task.worker.run

fn compute(value Int) Int {
    value * value
}

async fn main() {
    let left, right = (run(fn() Int { compute(6) }), run(fn() Text { "ready" })).await
    assert left == 36 && right == "ready"
}
```

See the [worker example](examples/workers) and run
`loom test compiler/std/task/worker`. List/Bytes operations and captured-cell
accesses protect complete typed values, bounds and backing storage. A sequence
of operations is **not atomic**: use the same `scoped` mutex around a compound
update in every participating caller. This also applies to multi-step source
library operations such as view indexing during source resize.

Existing Task joins, outcomes, deadlines and cancellation accept worker Tasks.
Cancellation is cooperative at generated checkpoints and mutex acquisition;
cleanup runs on the worker and drains before the parent continues. A completed
result wins late cancellation, and cleanup faults remain faults. Blocking
synchronous I/O parks the mutator but can delay cancellation until the OS call
returns. Scoped resources and owner-local Tasks/native tokens cannot cross the
worker boundary; acquire and close resources within the callback.

Shared builds revalidate mandatory contracts. Unprotected mutable shared entry,
`old` and current List observations are independent; scalar snapshots and observations of
fresh, unpublished Lists remain stable. Validated content-constrained List inputs
retain their entry values only when no admitted content mutation of that nominal
type occurs in the checked program. Construction requires non-publishing fresh
storage, and the checked write/escape policy supplies this evidence. It follows
refined fields inside records/tuples, not arbitrary read-only function bodies.
Length-only refinements retain their validated shape predicate after interference:
replacement-only inputs have stable lengths, while append-capable inputs have
independent length snapshots satisfying the predicate. Neither promises stable
elements; see the [shape example](examples/workers/shapes.loom).
This evidence does not imply private or disjoint storage and does
not remove runtime worker guards. The [constrained worker example](examples/workers/constrained.loom)
proves shared entry, length and bounded sorted-input properties. For unprotected inputs,
length nonnegativity is provable, but
two reads are not assumed equal and an earlier length check cannot justify a
later shared indexed access. Pure helper arguments and local bindings preserve
their once-only evaluation, including inline fields and guarded branches.
Checked clauses retain these identities across cache reuse; each invocation
imports a fresh scope. Reusing a scalar snapshot does not freeze its source.
Short circuits and hypothetical arithmetic still
need safety proofs. Private factory contracts compose through finite source
helpers. Checked allocation/return origins also recognize explicit scalar-element
copies from shared inputs, including source `std.list.clone` and inline wrappers.
Without postconditions, a factory supplies arbitrary typed storage, not guessed
lengths or contents. Shared elements stay shared; copying is not an atomic
snapshot under concurrent mutation. Multiple returned mutable fields may alias
and are not assumed disjoint. Publication through mutable graphs or opaque calls
conservatively loses privacy, including across loop iterations. A lock alone does not prove that all
aliases obey it; general synchronized heap reasoning remains unsupported.

Within worker-enabled builds, checked storage escape analysis removes access
guards for invocation-local List/Bytes/captured storage, including aliases and
fresh returned buffers. Containers, captures, branches, loops and cleanup all
contribute escape edges. Direct-call summaries compose publication, argument
aliases and returned storage to a fixed point, including recursion. A helper
parameter is private only when every incoming direct-call context is private;
mixed contexts keep one guarded body, without specialization clones. Callback,
dynamic and external entry parameters remain conservative. Unknown calls retain
guards; identity-only primitive forwarders use the same direct operations at
private call sites. This does not remove moving-GC roots or cancellation
checkpoints, infer ownership, or make compound operations atomic. Builds with
no workers retain their existing lowering.

## Source Tasks

For deadline-aware outcomes, use `std.task.deadline.at(task, deadline).await`
with an absolute process-local monotonic timestamp, or
`after_ns(task, duration).await` to compute one at the call. The source policy
requests cancellation, drains cleanup and returns the actual `Outcome`:
Completed results are preserved, cleanup faults remain Faulted, and successful
cancellation returns Cancelled. This is not a hard execution-time bound; running
OS calls and cooperative scheduling can delay return. `std.task.cancel_when`
provides the underlying source-level trigger composition. See the
[outcome example](examples/task_outcomes/README.md).

An `async fn` declares its logical result; calling it creates a hot `Task[T]`.
Both children below enter the ready queue before either body runs:

```loom
async fn twice(value Int) Int { value + value }

async fn main() {
    let first = twice(20)
    let second = twice(1)
    assert first.await + second.await == 42
}

test async fn computes() {
    assert twice(21).await == 42
}
```

Use `loom run` and `loom test` as usual; see the [task package](examples/tasks).
`.await` is a postfix keyword, allowed only inside async functions/tests.
`.await?` applies ordinary Result propagation to the completed value. Prefix
await, `.await()` and `.await!` are invalid. Task handles are one-shot: they
cannot be discarded, copied, overwritten while live or awaited twice.

The current executor runs a ready queue on one owner thread, not parallel
threads. Loom lowers suspension into typed constructor/resume functions and
GC-traced frames; ordinary functions keep their direct execution path. A child
fault propagates at await, and parent failure cancels and drains queued or
suspended descendants.

The [timer package](examples/timers) uses `std.time.sleep_ns`, `sleep_ms` and
`sleep_until_ns`; `std.time.monotonic_ns` supplies their process-local clock.
Relative sleeps begin when their task body runs, not when the call queues them.
Use `sleep_until_ns(monotonic_ns() + duration)` to choose a deadline at the call
site. Arguments and deadlines are evaluated once. The clock is not calendar time;
do not persist its values across processes. Timer notifications requeue suspended
tasks, and an idle owner blocks in a reactor created only on the first external
wait: no spin loop or thread per task. Scheduling promises no fairness, and
zero delays or past deadlines do not guarantee a yield.

The [suspended cleanup package](examples/async_cleanup) keeps `scoped` guards and
late-bound `defer` captures across real timer waits. Cleanup still cannot await
or use Tasks. The independent `std.resource.NoSuspend` marker forbids live values
across await, including aggregate members and pending operands; end their block
before awaiting. NoSuspend values cannot enter Task parameters/results or lose
their marker through dyn conversion.

Direct Task parameters, returns and nested results preserve one-shot obligations.
Generic `comptime if` may transfer these parameters in the selected branch:

```loom
fn forward[T](task Task[T], comptime direct Bool) Task[T] {
    comptime if direct {
        task
    } else {
        let moved = task
        moved
    }
}
```

An undetermined template selection retains pending Task states, not proof of
consumption. Concrete instances check their actual selected bodies before native
emission; missing, repeated or invalid transfers still reject. Required contracts
cannot use this pending state as an assumption. This adds no runtime selector.

Async concept/impl methods support concrete, generic and dynamic calls, default
bodies, associated results and type/comptime method parameters. The implementation's
async modifier must match its concept. Witnesses invoke the same typed constructors,
including creation-site diagnostics and Task argument adoption. Synchronous dynamic
methods can forward Task parameters/results without installing an owner. See the
[method example](examples/async_methods). MustScope receivers/parameters may be
borrowed by directly awaited calls, without transferring ownership. NoSuspend
parameters still reject. Fresh MustScope results stay owned by the completed
Task until one-shot extraction; abandoned results drain typed cleanup callbacks.
The receiver uses `scoped`, including after `.await?` for a fallible factory.
`Outcome`, `Result` and `Option` supply container hooks; only active MustScope
payloads receive recursive cleanup. Scoped or borrowed resources still cannot
be returned or transferred into another Task. Tuple `all`, `settled` and tuple
`.await` also retain resource results through partial construction, fault and
cancellation. Their complete results enter one `scoped` tuple; field reads borrow
its resources rather than extracting new owners. List and two-argument `any`/`race`
also support fresh resource results: selection retains the original typed winning
Task until every loser drains, then extracts its value or Outcome. A loser cleanup
fault also drains the untransferred winner. Dynamic List `all`/`settled` keep
resource results in their original producers until terminal notifications finish,
then construct the guarded result List in input order. Empty Lists, fail-fast
drain and cancellation use the same path as ordinary results; no per-input wrapper
Task is introduced. `cancel_when` and deadlines drain the trigger before transferring
terminal resource work; a trigger cleanup fault also drains untransferred work,
and an earlier work fault remains primary. `first_ok` likewise retains each typed
producer while inspecting only its enum discriminator. It drains losers before
transferring an Ok payload; all-error results use guarded List construction in
input order. Resources and nested Task payloads use the same source policy.
See the [native resource-result tests](examples/cleanup/task_resource_result_test.loom),
[tuple join tests](examples/cleanup/task_resource_join_test.loom), and
[selection tests](examples/cleanup/task_resource_selection_test.loom), plus
[dynamic List tests](examples/cleanup/task_resource_list_join_test.loom) and
[cancellation tests](examples/cleanup/task_resource_cancellation_test.loom) and
[first-Ok tests](examples/cleanup/task_resource_first_ok_test.loom).
Resource factory overloads
of [`std.stream`](std/stream/README.md) compose scoped async pipelines from
fallible or infallible factories. [`std.file.lines.stream`](std/file/lines/README.md)
opens lazily and uses bounded file workers for asynchronous line reads; its
pending pulls drain before lexical disposal. General borrowing Tasks that outlive
their call expression remain unsupported.
Named async functions can also be passed as ordinary function values:

```loom
async fn item(value Int) Int { value }
fn select() fn(Int) Task[Int] { item }
async fn main() {
    let callback = select()
    assert callback(7).await == 7
}
```

Synchronous Task factories use the same callable type. They execute inline;
async bodies run from the ready queue. Callbacks can be copied and stored in
records/Lists or returned through Tasks. Calling them retains owner requirements
and one-shot Task transfers; named callbacks are not Task obligations. Only
Task-returning callback signatures carry a private creation label, with no extra
runtime dispatch. Private coroutine entry/resume/cleanup operations retain their
direct native callback ABI. Async closures and capturing synchronous factories
use this same path; their environments cannot capture live Tasks or scoped
resources. Compile-time construction can retain these function references, but
cannot create, transfer or await real Tasks. Actual Task-bearing Lists use the transfer
operations described below. See the [callback example](examples/task_callbacks).

Tuples and records now transfer Task fields individually or as whole values:

```loom
async fn item(value Int) Int { value }
fn start() (Task[Int], Task[Int]) { (item(1), item(2)) }
async fn main() {
    let first, second = start()
    assert first.await == 1
    assert second.await == 2
}
```

Each Task field must be consumed exactly once, including nested fields and all
continuing branches. Whole-value transfer requires every field to remain available;
ordinary metadata can still be read after Task fields transfer. Bind a temporary
before extracting one field if other Task fields would be discarded. Async calls
adopt all parameter Tasks, and completed producers retain all returned subtrees
until extraction. This aggregate transfer mechanism is separate from tuple
`.await` and join APIs. See the
[aggregate example](examples/task_aggregates).

Enums, including `Option[Task[T]]` and `Result[Task[T], E]`, transfer through
ordinary `match` and `?`. Matching consumes the enum once and binds its active
payload; every bound Task must then be awaited or transferred. A wildcard can
cover remaining Task-free variants, but cannot discard Task-bearing payloads.
Async calls adopt and retain only the active variant's children, with no new
runtime representation or Task allocation for empty variants. See the
[enum example](examples/task_enums).

`std.file.tasks.read_bytes/read_text/write_bytes/write_text` return ordinary Tasks
and run open/create, read/write and normal close on a native pool of at most four threads per
owner. See the [file task package](examples/async_files). Source code owns I/O
loops, UTF-8 validation and close policy; workers copy buffers and publish completion
identities, never access GC objects. Cancellation drains active work before closing
resources. Unclaimed open results own their File until extraction or cancellation;
close transfers its private token exactly once. Duplication and failure-cleanup
close still block the owner, and cancellation can wait for a stuck OS call.
Private async intrinsics must be awaited directly and suspend the current frame,
without creating another Task. The synchronous `std.file` API remains unchanged.

`std.process.tasks.capture` and `capture_input` return Tasks with the same
`Result[Output, SpawnError]` and optional child-local `Options` as synchronous
capture. The native wait snapshots arguments, input and options once; both
binary output streams are drained concurrently and copied back on the owner.
Cancellation discards queued work or drains a running capture and reaps its
direct child; it does not kill process trees or impose a hard timeout. The same
four-worker pool bounds concurrent jobs, but active captures also use pipe
threads. Output remains unbounded buffering, not streaming. See the
[process example](examples/async_processes/README.md).

`std.net.tcp` provides a narrow numeric-address TCP path: await `listen` to bind
an IPv4 or IPv6 address, find an ephemeral bind's `local_port`, then await
`accept` or `connect`, `read`, and `write_bytes`. `connect` starts a nonblocking
socket, returning immediately on success or awaiting writable/error readiness
before checking socket-error/peer state. Failed completion remains terminal;
failure or Task cancellation closes its socket.
`read` appends to a shared Bytes buffer;
`Ok(0)` means EOF for a positive limit. `write_bytes` retries partial and
WouldBlock writes until its initial buffer length is sent. Bytes contents are
not snapshotted: alias mutation during a pending write can change existing data
or cause a write error, but appended bytes stay outside the initial write end.
`Listener` and
`Stream` contain private owner-local token state, never a raw descriptor.
Copying a wrapper aliases its identity; `close_listener`/`close_stream` revoke
all copies and reject close while a pending or delivered readiness wait leases
the socket. Use `defer` to close after suspension, and cancel/drain child waits
before closing a shared socket. The [loopback example](examples/tcp_loopback/main.loom)
exercises O0/O2, forced GC, close, and cancellation.

`std.net.udp` provides numeric IPv4/IPv6 `bind`, `local_address`, `receive` and
`send`. Each received `Datagram` contains fresh Bytes and the numeric sender;
empty packets are data, not EOF. `receive(socket, limit)` consumes one packet
and reports `UdpError.Truncated` instead of returning a partial oversized packet.
The default limit is 65535 bytes; limits outside 0–65535 reject. `send` retains
the initial byte extent across readiness retries and never splits a packet;
callers synchronize aliases that can modify those bytes. UDP does not guarantee
delivery or ordering, and the OS may reject large payloads. Copies share an
owner-local socket identity. Drain pending I/O before `close`; `abort` revokes
aliases and wakes waits to fail. This uses the same reactor and Task cancellation
as TCP, without DNS or a separate executor. `connect(numeric_peer).await` returns
a `Connection` with an ephemeral local port and fixed peer. It has no handshake
or reachability/delivery guarantee. `send(connection, bytes)` sends one packet
to that peer; `receive` uses kernel peer filtering. Connection copies share the
same close/abort identity; `peer_address` and `local_address` expose numeric
endpoints, not descriptors. `set_broadcast(socket, enabled)` explicitly controls
IPv4 broadcast permission for an unconnected Socket; `broadcast(socket)` queries
it. It defaults off, is shared by aliases, and does not guarantee routing or
delivery through the host's network policy. `join_multicast(socket, group, interface)`
and `leave_multicast` manage shared membership: IPv4 takes numeric group/interface
addresses ("0.0.0.0" for OS selection), while IPv6 takes an unbracketed group
without a zone and an interface index (0 for OS selection). Socket/group families
must match. Invalid input, stale identities and OS failures return
`UdpError.Membership`. Closing releases memberships; repeated operations are
not promised idempotent. `set_multicast(socket, MulticastV4 { ... })` or
`MulticastV6` configures `interface`, `loopback` and `hops` (0–255: IPv4 TTL or
IPv6 hop limit), separately from membership. Configuration is shared and does
not retire waits. The OS can reject valid requests, including default-interface
selection; failure can leave options partly applied. Close if exact policy is
required. The example uses an explicit loopback interface and TTL zero for a real local IPv4 multicast exchange,
including delayed readiness and a draining deadline. This does not establish
routed multicast delivery or IPv6 delivery under host interface policy.
`connect(host, port).await` uses the existing async DNS resolver and chooses the
first endpoint accepted by the OS in resolver order, without probing or racing
remote peers. Lookup errors return `UdpError.Resolve`; invalid peer ports and
exhausted endpoints return `UdpError.Connect`. Numeric-only calls omit the
resolver operation from native IR. Task deadlines can bound lookup, with running
OS lookup drainage on cancellation. See the [UDP example](examples/udp_echo).

`std.net.dns.resolve(host, port).await` returns numeric socket addresses in OS
resolver order, including hosts-file entries, as `Result[List[Text], ResolveError]`.
`std.net.tcp.connect(host, port).await` interleaves address families, preserving
the first family's preference and order within each family. Defaults allow two
pending attempts, start the second worker after 250 ms, and request cancellation
after 30 seconds including DNS. Failed attempts advance immediately through a
shared address queue. `connect(host, port, ConnectOptions)` overrides
`attempt_delay_ns`, `timeout_ns` and `parallelism` (1–16); `connect_options()`
returns the defaults. `connect_any(addresses, options)` uses the same policy on
a snapshot of numeric endpoints. Losers, including already-completed sockets,
close before returning the winner; cancellation drains waits before closing.
Resolution failures return `TcpError.Resolve`, exhausted attempts `Connect`,
deadline cancellation `Timeout`, and invalid options `Option`.
The single-argument `connect` and `listen` still take only numeric `host:port`
or `[IPv6]:port` addresses and add no deadline.
DNS shares the bounded file I/O worker pool and completion queue. Native workers
never retain managed pointers; cancellation drains any running OS lookup before
cleanup. See the [hostname example](examples/hostname_connect).

`local_address(listener)` / `local_address(stream)` and `peer_address(stream)`
return numeric endpoint Text (`host:port` or `[IPv6]:port`), without DNS or raw
descriptor access. `local_port` also accepts a Stream. `set_nodelay(stream, Bool)`
controls TCP_NODELAY, not application buffering. `shutdown_write(stream)` signals
EOF after bytes already accepted by TCP, retaining the receive side and pending
read registrations. It is synchronous and does not revoke the token; finish
intended writes first and still call `close_stream` after task waits drain.
Already-pending or later writes may fail. Repeated half-close success is not
promised across operating systems. These operations report `Address`, `Option`
or `Shutdown` errors, including stale tokens. See the
[EOF-delimited request/response example](examples/tcp_half_close/README.md).

`set_keepalive(stream, Option[KeepAlive])` configures idle connection probes.
`Some(KeepAlive { idle_seconds = 60; interval_seconds = 5; retries = 3 })`
enables them; `None` disables them. All fields must be positive, with whole-second
durations. Unrepresentable values reject instead of silently narrowing; host
limits may reject otherwise valid configurations with `TcpError.Option`.
Options apply to all aliases and preserve the socket and pending I/O. An OS
failure may leave a partially applied configuration; close the stream when an
exact policy is required. Keepalive is neither an application heartbeat nor an
I/O deadline; use Task cancellation/deadlines for the latter. Connections keep
their OS defaults until explicitly configured.

`abort(stream)` revokes all aliases and retires active socket waits. Pending I/O
wakes to fail, including a wait Task that has not started yet. Already-sent bytes
are not rolled back; callers must still consume or cancel their child Tasks.

`std.net.tls` provides `Connection`, `ClientOptions`, `ServerOptions`, `Trust`,
`Identity` and `ClientAuth`
and typed errors over TLS 1.2/1.3. `connect(host, port)` verifies the host against compiled Mozilla
roots. The options overload accepts explicit PEM trust roots and ALPN; numeric
`connect(address, name, options)` separates the TCP endpoint from the verified
DNS/IP name. `Identity` holds a PEM certificate chain and private key.
Client options select `Option.None` or `Option.Some(identity)`; server options
require an identity and select `ClientAuth.Anonymous` or
`ClientAuth.Required(client_ca_pem)`. Required authentication rejects missing,
untrusted, expired and wrong-purpose client certificates before accepting the
connection. Empty/malformed configuration fails before transport I/O; it does
not silently select anonymous access. Client and server trust roots are separate.
There is no insecure verifier or system-root lookup. Built-in roots update when
the toolchain is rebuilt with a newer root package.

Await `read`, `write_bytes` and `shutdown_write`; call `close` in `defer`.
Reads append bytes, clean TLS EOF returns zero, and truncated TCP EOF fails.
Write shutdown sends TLS close-notify, not TCP FIN, and preserves receiving.
This preserves the local receive side, not a promise of more peer data; TLS 1.2
peers may close both directions. EOF-delimited request/response protocols should
use TLS 1.3 or an explicit application message delimiter.
Copies share one connection: one reader and one writer can run concurrently;
same-direction overlap returns `Busy`. Encrypted output retains order, and an
active writer does not block reads behind its outgoing backpressure. Drain child
Tasks before closing; failed/cancelled operations retire the connection and wake
pending operations to fail, so partially sent records cannot be reused. Payload
contents share TCP's alias rules above.
`protocol`, `local_address` and `peer_address` query ALPN and numeric endpoints.
`peer_certificate` returns a fresh DER copy of the verified leaf as
`Result[Option[Bytes], TlsError]`; anonymous clients have no leaf. Certificate
authentication does not assign application roles or interpret client names.
No CRL/OCSP policy is exposed yet; trusted issuance is not a revocation guarantee.
Resumption configuration remains open. See the [TLS example](examples/tls_loopback/README.md).

The source package owns this policy; Rustls supplies packet processing in the
optional `libloom_tls.a` (`loom_tls.lib`) beside the core archive. Native linking
inspects actual object references, so unused TLS imports and ordinary Tasks do
not retain cryptography. Cached objects use the same selection and relink with
the current provider. `LOOM_RUNTIME_LIBRARY` selects the core archive and its
directory; TLS programs require the named sibling provider.

`std.task.deadline` can cancel a connect/read Task; DNS cancellation still drains
running OS resolution. There is no DNS cache,
structured OS-error detail or full socket-option surface yet.
General worker operations also remain open; the
[accepted design](../docs/rfcs/tasks.md) remains broader.

`std.list.transfer.append(values, value)` returns the same shared List header;
`take_last(values)` returns `Option[(element, remaining List)]`. This supports
dynamically sized Task groups without copying one-shot elements. A known empty
Task-bearing List must also be transferred or consumed. Ordinary get/index/push/set
and length queries do not borrow Task-bearing Lists; use the transfer API.
Normal Lists still share mutations, including at compile time. See the
[dynamic Task list example](examples/task_lists) for iterative draining and
recursive payloads. `replace(values, index, replacement)` returns the displaced
element and the same updated header, with bounds checks and compile-time support.

Private completion primitives now register each child once and deliver terminal
indices through the same typed suspension path, preserving actual completion
order and one-shot result extraction. They retain IDs, not managed pointers;
parent cancellation removes observations before cleanup. `loom test compiler/std/task`
exercises the native source path. `std.task.outcome(task).await` now returns
typed Completed/Faulted/Cancelled data, with ordinary Result errors treated as
completed values. `std.task.cancel(task)` consumes and drains a child before
returning its actual terminal outcome; it can block the owner while native work
finishes. Already-completed results and Task-valued payload obligations survive.
No-result Tasks match `Outcome.Completed(_)`, without source Unit syntax.
See the [outcome example](examples/task_outcomes). List `std.task.all/settled/any/race`
now use one-time completion registration and indexed transfer. They support
dynamic counts, no-result payloads and returned Tasks, draining losing subtrees
before return. Cleanup faults fail an otherwise successful join; existing primary
faults retain precedence. See the [join example](examples/task_joins).
`std.task.first_ok` accepts `List[Task[Result[T, E]]]`, skipping ordinary errors
until an `Ok` arrives. If all fail, it returns errors in input order; empty input
returns `Err([])`. Execution/cleanup faults still fail the join. Fresh MustScope
Ok/error payloads stay in the original producers until selection finishes;
unselected resources drain before transfer and the caller scopes the complete
result. Dispose-only values still require explicit resource policy, as in TCP.
Heterogeneous tuple `all/settled` accept arbitrary arity and preserve input
order without a per-element helper chain. A statically typed tuple of Tasks can
also use `.await`, which follows source `std.task.all` policy.

## Next boundary

The [native basic benchmark](../benchmarks/basic/README.md) compares the current
compiled path with C, Go, Rust and Zig, including separate integer-checking modes.
It is distinct from the compiler-latency measurements above.

The `compiler/examples/data` package exercises records, enums, generic functions,
and both test forms through the same CLI. Scalar-only records stay native values;
enum storage uses its largest variant payload, not the sum of all variants.

The [Loom-written compiler](loom/README.md) uses ordinary source packages for
syntax, project loading, binding, checking, proof, and typed artifact emission.
It checks its own sources and builds subsequent native stages. The compiler and
independent user packages share the public
[`std.loom` syntax libraries](loom/README.md#public-syntax-libraries) and opt-in
[project/binding APIs](loom/README.md#public-project-and-binding-libraries).
[Typed analysis](loom/README.md#public-typed-analysis) adds checked expression
types and concrete call targets without invoking the compiler CLI or backend;
declaration binding alone still provides only name candidates.

The runtime uses single-threaded stop-the-world copying GC with bump-allocated
pages for small objects and a large-object space. Small reachable allocations
move, roots and typed fields are rewritten, then obsolete pages are freed.
Large allocations are traced in place to avoid
copying whole backing buffers before growth; this is not a stable-address promise.
Aliases and cycles retain their meaning; static Text literals do not move.
No source-visible address, ownership syntax, finalizer or pinning obligation is
introduced. Collection temporarily holds both old and copied storage.
LLVM first
inlines source calls with opaque root-region markers, then lowers one linked
stack-root frame per remaining native function. Region exits clear inactive
slots; no marker reaches object code and no root table is copied on entry.
Optimized builds inline direct primitive forwarders with small scalar guards
before committing frames, so a checked `std.bytes.push` or `std.list.push` does
not register a separate frame on every loop iteration. Larger wrappers and
indirect calls retain ordinary LLVM inlining decisions; O0 stays unforced.
Transitively nonallocating functions need no frame. Temporary snapshots protect
managed results only across a later possible allocation; immediate local/return
handoffs and nonallocating reads need no temporary root. Pending arguments and
aggregate fields keep independent snapshots, while completed expressions and
mutually exclusive branches reuse same-type slots. A match binding whose only
use is immediate result handoff needs no local root; the enclosing expression
still protects that result across later allocations. Backward checked control-flow
liveness clears dead local shadows at block entry and after complete statements,
including branches, loop backedges and break/continue paths. Pending operands
retain separate snapshots. Cleanup captures remain function-wide, and callbacks
never clear borrowed owner slots; finer cleanup and expression-internal retirement
remain future work. Retirement is omitted when no later collection can benefit.
Small objects carry private allocation/forwarding metadata beside their payload;
page maps and base bitmaps replace per-object hash entries. Large objects remain
separately tracked. Neither mechanism changes source layout or native callback ABI.
Ordinary locals remain nonescaping SSA candidates; separate shadow slots mirror
their source writes for the collector, including pattern bindings. After a
possible allocation, used locals and pending expression snapshots reload updated
references. An earlier argument retains its own value even if a later argument
reassigns the source variable.
List/Text accesses use checked typed loads/stores, not runtime accessors.
Source `std.list.view` adds fixed-shape ranges with shared element identities;
see the [shared view example](examples/shared_views). A watched List removal
retains one cell per removed identity for overlapping views. An ordinary List
removal has only a registration-flag branch; reads and writes are unchanged.
Internal weak registrations are pruned after GC tracing, not language-level
weak references. Views retain their source and any detached cells they need.
List/Bytes push calls the runtime only on capacity growth, then reloads the
backing pointer before publishing the initialized element. Raw spare capacity
is uninitialized and never traced; managed headers/payloads still start zeroed.
Growth copies page-backed storage or reallocates individually owned backing
storage and updates the shared header;
source-visible interior pointers cannot survive this boundary. File reads
initialize their requested range before passing a slice to Rust I/O.
`LOOM_GC_STRESS=1`
collects before every allocation and relocates all sizes for focused testing.
The LLVM tool links managed
programs with `libloom_runtime.a` (`loom_runtime.lib` on Windows) beside it, or at
`LOOM_RUNTIME_LIBRARY`.
Whole-file helpers close explicitly on normal returns and defer closure on faults
or worker cancellation. Lexical
resource cleanup is described separately above; neither mechanism uses GC finalization.

The N0 source-to-native gate is exercised by the examples and integration tests.
N1 now includes the complete source frontend for this subset and native staged
bootstrap through the retained LLVM tool. Replaced Rust source-language stages
and historical checkpoint configuration are absent from the active tree.
Stage numbers denote bootstrap generations, not
language versions. The bootstrap subset limits how the compiler source is
written, not what language features the resulting compiler can offer users.
Broader proofs and mutable-alias preservation, general resource transfer into
Tasks, broader worker interference proofs, remaining pack combinations,
and complete incremental coverage remain open.
Semantic-change and deployment tools have bounded working
prototypes, not their general accepted workflows. See the concise
[status](../docs/project/implementation-status.md) and
[roadmap](../ROADMAP.md). No complete language or `std` claim is made.

Unsupported syntax and manifest features reject explicitly. In particular,
unsupported dependency sources and target declarations are not silently ignored. The accepted
[language foundation](../docs/rfcs/language-foundation.md) remains the goal;
these temporary limits do not redefine it.

For this compiler's local gate:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
bash scripts/bootstrap.sh --dev
cargo test --locked --workspace
```
