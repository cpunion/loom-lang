# Native compiler

The [Loom-written compiler](loom/README.md) implements package loading, parsing,
binding, type checking, bounded required proofs, and checked program emission.
It builds further compiler stages using one retained Rust LLVM/platform tool.
The [roadmap](../ROADMAP.md) distinguishes the native bootstrap from completion
of the accepted language. A previous Loom compiler is the bootstrap input;
the frozen historical Rust seed is only a fallback for producing that input.

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
and Clang. macOS, Linux, and Windows
pass the LLVM 22 bootstrap and native gate. On Ubuntu 24.04 use the signed
[LLVM apt repository](https://apt.llvm.org/) and install `llvm-22-dev`, `clang-22`,
and `libpolly-22-dev`; set `LLVM_SYS_221_PREFIX=/usr/lib/llvm-22` and
`LOOM_CC=/usr/bin/clang-22`. The [CI recipe](../.github/workflows/ci.yml) shows
repository setup and runs the same full native/bootstrap gate.
From the repository root on macOS:

```sh
brew install llvm@22
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
and publishes `target/loom`. On macOS/Linux, a cold build starts with the frozen
Rust seed in `compiler/bootstrap/seed`, then builds the ordered Loom source
commits in `compiler/bootstrap/checkpoints`. Each checkpoint implements a
capability before the next compiler uses it: native Float before evaluator
storage, function values before higher-order `std.list`, and native I/O
primitives before their public wrappers. These immutable inputs are
cached in `target/bootstrap/<commit>/`; no preinstalled Loom compiler is required.
Pinned commits must be available in Git history; the script reports an exact
fetch command when one is missing. The cache is disposable, not another
maintained frontend.

The frozen source seed also uses LLVM 22. Its toolchain-only update changes the
Inkwell feature and lockfile, not the historical compiler source. A cold build
does not require LLVM 19, rewrite dependency files, or resolve an unlocked build.

An existing compatible Loom compiler bypasses historical seed recovery:

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
compiler; on macOS/Linux, a missing installed compiler uses the historical fallback. This
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
Native executables reserve an 8 MiB main stack, with the default commit size,
so bounded compiler recursion does not inherit MSVC's smaller 1 MiB default.

Windows cannot use the frozen historical Unix seed directly. A fresh checkout
contains a compressed, source-bound checked stage 0 in
[`compiler/bootstrap`](bootstrap/windows-stage0.source). Git Bash verifies its
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

The checked stage 0 is generated from the immutable source commit recorded in
`compiler/bootstrap/windows-stage0.source`. The generator normalizes only
source-location prefixes, preserving every encoded byte length. macOS and Linux
CI independently emit it from that pinned source with the normal Loom type and
proof checker, then compare every byte with the committed input. To verify or
intentionally refresh it after a checked-artifact/backend ABI change, run on
macOS or Linux with a working compiler:

```sh
node scripts/windows-bootstrap-seed.mjs --check target/loom
node scripts/windows-bootstrap-seed.mjs --write target/loom
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
Only successful checks are published. Each `checked-v2` entry contains metadata
and checked bytes under one SHA-256 checksum; `definitions-v1` binds its source,
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

Each pair checks identical copied source after changing one private helper.
The cached variant must miss the whole-closure cache and reuse definitions and
bodies. Initial misses, alternating sample order, time, peak RSS and reuse counts
are recorded separately. This isolates a local edit; it does not represent a
public API change. Snapshot I/O and copying can outweigh saved checking, especially
for small programs; a positive reuse count alone is not evidence of a speedup.

The [2026-10-01 paired comparison](../benchmarks/compiler/results/2026-10-01-macos-arm64-one-shot-snapshot.json)
measured the same edited inputs with two O2 compilers and independent caches:

| Edited package | Before | One-shot snapshots |
| --- | ---: | ---: |
| Data example | 28.79 ms | 28.98 ms |
| Compiler | 1,824.85 ms | 1,569.39 ms |
| 200 generated helpers | 74.21 ms | 66.50 ms |

The compiler case reused the same 1,739 definition checks and 2,833 bodies.
Removing copies needed only by resident hosts reduced latency about 14% and
peak RSS from 1,362 to 1,150 MiB (about 16%). These are five alternating
fresh-process macOS arm64 medians, not arbitrary dependency changes or native
build speedups. Disk restoration and writeback still cost memory and time;
positive reuse counts alone do not justify enabling caching by default.
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
- Source `std.float` supplies explicit integer conversion, finite/NaN queries,
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
- Source `std.io.read_bytes()` reads stdin to EOF; `write_bytes(Bytes)` writes
  stdout, and `write_error(Bytes)` writes stderr. These preserve arbitrary bytes,
  report byte counts/errors, and never close standard streams. `read_text()` still
  rejects invalid UTF-8. These whole-input reads buffer to EOF;
  `read_chunk(buffer, limit)` instead appends up to a positive byte limit and
  returns the count (zero at EOF), allowing interactive framed input without
  closing stdin. See the [binary filter](examples/binary_streams/main.loom).
- Source `std.hash.sha256.digest(Bytes) Bytes` produces a fresh 32-byte digest;
  `hex(Bytes) Text` hashes input and returns its 64 lowercase hexadecimal digits.
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

An ordinary top-level function can declare one final type pack. Structural
tuple, function and nominal parameters may expand it at any parameter position;
a direct variadic value
parameter must be last. Type patterns expand elementwise; the named value pack
is an immutable tuple.

```loom
pub fn pack[Ts...](values Ts...) (Ts...) {
    values
}

fn forward[R, Ts...](callback fn(Ts...) R, values Ts...) R {
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
tuple or record binding. This includes the final value pack of a variadic
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
parameter. It may have fixed fields around one expanded pattern, such as
`values (Int, Pattern[Ts]..., Text)`. The pack arity is the argument's statically
known tuple width minus the fixed fields, including for named tuples, function
results and contextual function references. A zero-element pack retains the
fixed fields. Ordinary checking validates those fields and every expanded
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

Unselected arities have not had their bodies verified. Variadic `ensures`
declarations currently reject even when uncalled: proving selected arities is
not a proof for every arity. Preconditions currently use fixed scalar parameters,
not the tuple pack, and retain ordinary checked/runtime boundaries.
Concept methods accept the same final type pack, including defaults, overrides,
ordinary generic implementation parameters, structural inference and `async`.
Implementations may rename the pack and inherit its element requirements, but
cannot strengthen them. Conformance compares expansion-preserving type schemas,
not a few sampled arities. A selected arity becomes an ordinary method slot;
`dyn` dispatch retains only concrete slots used by the build. `Self` remains
receiver-only in dynamic methods, even inside an empty expansion. See the
[method-pack example](examples/variadics/methods.loom).

Impl headers also accept one final pack. The target determines its arity:

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
use ordinary inference after expansion. Selected arities check every method
and associated binding with abstract elements and declared requirements, not
only the caller's concrete types. `comptime for T in Ts` uses the same type
iteration as function packs. A method-local pack has an independent arity;
defaults, overrides, CTFE, dyn slots and native Task payloads stay ordinary.
Incremental recipes retain source identity and both arities, not generated impl
indices. See the [impl example](examples/data_packs/implementations.loom).

Overlapping impl families reject before a call selects an arity. Fixed
constructors and anchored prefix/suffix patterns prove disjointness; unknown
intersections reject conservatively. Bounds or sampled arities do not prove
disjointness. An impl family's ordinary method contracts are proved at each
selected arity; unselected arities have not had their bodies verified.

Multiple packs in one parameter list,
general type-list reflection and richer pack iteration remain open. This
implementation does not complete the accepted metaprogramming design.

Records and enums accept one final type pack too:

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
Bool, Text and inline record/tuple value facts. Unspecified leaves remain independent;
shared siblings supply no content or alias facts. Summaries never guess an
implementation or enter native code. See the [generic](examples/concept_contracts/generic.loom)
and [aggregate](examples/concept_contracts/aggregates.loom) contract examples.
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
the refined type directly. Closed literals with `Float`, Text, tuple or enum fields can also
return directly when compile-time evaluation establishes its predicate; a false
constant is a diagnostic. Unknown inputs, calls, and failed optional evaluation
retain the `Result` boundary.
See the [record refinement example](examples/record_refinement/main.loom).

List-backed constraints can observe lengths and contents when their element
values are immutable scalars, Text, or inline records/tuples/enums. Construction
must use a fresh List literal or a pure factory proved not to return or publish
input aliases. Explicit source `std.list.clone` is one such factory, not a
compiler-recognized public name:

```loom
type PositiveValues = List[Int] where all_positive(self)

fn checked_copy(values List[Int]) Result[PositiveValues, ConstraintError] {
    PositiveValues(clone(values))
}
```

Here `all_positive` is an ordinary pure function; see the complete
[List constraint example](examples/record_refinement/lists.loom). A proved
literal returns `PositiveValues` directly; other inputs keep the checked Result
boundary. Immutable aggregate elements use the same rule, including Text and
enum payloads; see the [literal example](examples/record_refinement/literals.loom).
`PositiveValues(existing_list)` rejects: construction cannot silently
change that List's existing writable aliases. No automatic copy or runtime
monitor is installed. Copies of a constrained value keep sharing its storage.

Indexing, ordinary non-escaping read helpers and explicit copies are allowed.
When the predicate observes only length, element replacement is also permitted:

```loom
type IntPair = List[Int] where length(self) == 2

fn replace_first(pair IntPair, value Int) {
    pair[0] = value
}
```

Aliases observe that replacement. Source helpers such as `set` and `reverse`
work by the same inferred effects, without special library-name rules or a new
check after each write. Appending additionally requires a static proof that
the predicate at length `n` implies it at `n + 1`, including helper preconditions
and arithmetic definedness. Thus `length(self) > 0` permits `push` and source
append helpers, while fixed length and upper bounds do not. Capacity/length
overflow faults before mutation. Unsupported proofs do not grant permission.
A predicate that observes elements keeps the read-only capability; unknown effects, raw alias returns and
publication into another aggregate reject at compile time. Factories
and borrows are checked through helper bodies, not trusted annotations. These
rules also run for compile-time code and unused concrete functions. The constrained
List has the ordinary native List layout and survives moving GC and Task handoff.
Automatically proving that selected writes preserve an arbitrary predicate,
and strengthening pre-existing writable aliases, remain later analysis work.

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
proofs retain ordinary checked `Result` construction. General
Float postcondition reasoning remains unsupported.

Construction also consumes already established facts about an immutable scalar
local or parameter, from `requires`, a successful `assert`, or the current
`if`/`while` branch:

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
with immutable locals, `if/else` and early body returns. It uses each checked
specialization in its defining scope and preserves eager arguments, unused
calculations, short-circuit guards and helper preconditions as proof obligations.
For example, replacing `self >= 0` above with `nonnegative(self)` can still remove
the check when `nonnegative` returns `value >= 0`. A helper returning `true` after
`let unused = value + 1` cannot discard a possible overflow. Unsupported helper
loops/mutation, indirect calls or exhausted expansion retain runtime checks.
General implications over shared mutable contents remain open; supported
immutable input invariants and flow facts are described below.

`requires` is checked before the callee body. Every declared `ensures` must be
proved; unknown or unsupported proofs reject the build, including for functions
outside the emitted entry closure. There is no runtime postcondition fallback.

Function contracts can reuse direct, acyclic helpers over scalars and inline
records/tuples, with immutable locals and conditional bodies. The [contract example](examples/contracts/README.md)
shows a predicate with its own `requires`: proving its returned Boolean alone is
not enough; the caller must also establish that requirement. Expansion preserves
multiple parameters, branch/short-circuit guards, eager arguments and unused
arithmetic. Scalar choices normalize into bounded Boolean clauses for contracts;
body-call proofs reuse typed branches. Assertions inside a helper are obligations
in a postcondition, not assumed facts.
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
Concatenation and substring reasoning remain unsupported. See the
[native Text example](examples/text_contracts).

List lengths are stateful, unlike immutable Text lengths. `std.list.length`,
List literals, `std.list.new`, indexing, `std.list.get/set` and `std.list.push`
participate in required proofs. Exact local aliases share the same extent;
append updates it and forgets other possibly overlapping extents. Indexed reads
return fresh unknown elements and preserve length; writes establish no content
facts. Successful accesses retain ordinary bounds checks and normal-return
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
during an actual sorting loop, **not** sortedness or permutation. General element
relationships, mutable `old` snapshots and List-local reassignment in proof loops
remain unsupported. These proof states add no native object metadata.

Aggregate summaries compose through nested calls, field projections and
whole-value updates. Unconditional proved equalities such as
`ensures result.count == value.count` preserve that field's exact value;
unspecified fields and independent calls retain separate unknowns. A weak
summary does not inherit stronger facts from its implementation. Shared fields
can pass through an aggregate but their contents remain opaque; this is not a
shared-state invariant or alias-mutation proof. Predicate helpers still evaluate
every argument and initializer, including an unused field that could overflow.
`old` denotes immutable entry expressions: parameter paths, immutable aggregates,
arithmetic and finite pure helpers can compose (`old(value).count`,
`old(identity(value)).0`). Every operand must be entry-derived and immutable;
an immutable field can be selected beside an unobserved shared sibling, but
`old` of a whole aggregate containing shared mutable storage rejects. Helper
arguments and predicate arithmetic still require definedness, even when a
helper ignores an argument. No entry computation or snapshot allocation enters
native code. Callback parameters, result/body-local references and shared-data
snapshots remain unsupported; this is not general heap-entry reasoning.
See the [aggregate contract example](examples/aggregate_contracts/main.loom).

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

`while` bodies with Int/Bool locals or scalar-only inline records/tuples can
prove normal-return contracts through inferred invariants, without new annotations.
The prover freshens every scalar leaf of written locals and proposes per-path
entry/range bounds and weakened relational guards. Whole-value reconstruction
does not retain stale sibling fields; shared containers and other unsupported
leaves are not admitted as written aggregate locals. Each candidate
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
Shared contents remain opaque. Int/Bool predicates use the existing bounded
fragment. If direct facts are insufficient, acyclic pure predicate helpers expand
in a private checked closure, retaining their guarded preconditions and successful
checked calculations. This does not add runtime calls or change construction
checks. Unsupported conjuncts, helper loops/recursion and Float arithmetic supply
no evidence. See the [input invariant example](examples/invariant_contracts).

The current proof fragment supports scalar linear arithmetic, comparisons,
Boolean facts, local assignments, branches/returns, and the scalar loops above. It reasons from
preconditions and successful checked operations. A source-written `assert`
provides a fact only after that assertion succeeds; the compiler never inserts
an assertion to rescue a failed postcondition proof. Postcondition arithmetic
must itself be defined within `Int` bounds.

Helper loops/mutation, returns inside helper operands, recursive proof dependencies,
dynamic calls without a usable declared scalar contract, indirect calls,
nonlinear arithmetic and nonconstant division remain outside this
proof fragment. Required Float proofs remain unsupported, while pure Float entry
predicates can run normally. Solver work is bounded; exhaustion is a diagnostic,
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
buffers. Scalar values become constants; containers are allocated and populated
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
results may appear inside shared aggregates. Required Float proofs remain
unsupported and reject; successful evaluation is not an algebraic proof.

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

Nested record resources now receive pending cleanup during construction.
Generic parameters and associated fields can use `Dispose + MustScope` bounds
for the same transfer. Concrete specialization registers every nested resource,
not just the abstract field's outer Dispose method; completed fields still drain
if a later initializer or a cleanup faults. See the
[generic cleanup tests](examples/cleanup/generic_resource_test.loom).
Later fields may create closures or use field-local loops: a closure's return
and a local loop's break/continue do not escape aggregate construction. Discarded
`comptime if` branches do not participate in this check. Actual
enclosing returns, propagation, loop exits and awaits still reject while a
resource field is pending.
Enum payloads use the same pending transfer and LIFO cleanup, selecting only the
active variant. An enum used with `scoped` must implement Dispose; its MustScope
payloads are cleaned automatically after the enum's own Dispose method. Matching
that scoped enum or a borrowed receiver borrows payloads, including nested and
guarded patterns. It does not permit copying, returning, manually disposing, or
putting those payloads into another `scoped` binding. See the
[enum cleanup tests](examples/cleanup/resource_enum_test.loom).
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
or manually dispose resource elements. Synchronous MustScope parameters are
checked borrows, including indirect calls; callers must scope fresh arguments
first. No borrowing syntax is required. See the
[List cleanup tests](examples/cleanup/resource_list_test.loom).
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
[method example](examples/async_methods). Async parameters cannot contain MustScope
or NoSuspend values, and scoped receivers cannot escape into child Tasks.
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
returns `Err([])`. Execution/cleanup faults still fail the join. Discarded
completed values do not close external resources; resource-producing races need
explicit cleanup, as in the TCP source policy.
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
still protects that result across later allocations. Other locals remain
function-wide roots; general local liveness remains future work.
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
Whole-file helpers close handles explicitly on recoverable branches. Lexical
resource cleanup is described separately above; neither mechanism uses GC finalization.

The N0 source-to-native gate is exercised by the examples and integration tests.
N1 now includes the complete source frontend for this subset and native staged
bootstrap through the retained LLVM tool. Replaced Rust source-language stages
are absent from the active tree; the pinned historical fallback is not an
old-language support policy. Stage numbers denote bootstrap generations, not
language versions. The bootstrap subset limits how the compiler source is
written, not what language features the resulting compiler can offer users.
Broader proofs and mutable-alias preservation, general resource transfer into
Tasks, general worker APIs, remaining pack combinations,
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
