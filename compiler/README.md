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

Use Rust 1.88, LLVM 22 development libraries, and Clang. macOS, Linux, and Windows
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

The root Cargo workspace alone builds `loom-native` and the runtime, not the
public `loom` compiler. The source compiler defaults to `compiler/std` and
`target/debug/loom-native` relative to the working directory; `--std` and
`--native-tool` select explicit paths. These are development commands, not a
relocatable release package or a stable compiler-artifact ABI. `--help` lists
each tool's command surface.

`loom test --no-run` produces the native test executable without running it;
`--output` optionally selects its path. Both forms of source tests and normal
test-only imports remain included. A package with no tests reports `0 tests`
without creating a binary. Ordinary `loom test` still compiles and runs its tests.

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

Windows cannot use the frozen historical Unix seed directly. Use an existing
compatible Windows compiler via `LOOM_BOOTSTRAP_COMPILER`, or export a trusted
checked compiler from the same checkout using an already validated macOS Loom:

```sh
target/loom emit-checked compiler/loom > target/compiler.checked
```

Transfer that file to the matching Windows checkout, then in Git Bash with the
Visual Studio environment inherited:

```sh
export LLVM_SYS_221_PREFIX='C:/llvm-22'
export LOOM_CC="$LLVM_SYS_221_PREFIX/bin/clang-cl.exe"
LOOM_BOOTSTRAP_INPUT=compiler.checked bash scripts/bootstrap.sh
target/loom.exe test compiler/examples/scalar
```

The native Windows bridge builds stage 0 from this checked input, then Loom
builds stages 1/2/3 and compares 2/3. The result is `target/loom.exe`; default
program/test outputs use `.exe`, library objects `.obj`, and the runtime archive
is `loom_runtime.lib`. Subsequent local edits use `bash scripts/bootstrap.sh --dev`.

`emit-checked` runs normal type/proof checks and writes the private artifact to
stdout without invoking LLVM. This is not a stable IR ABI, release artifact,
or committed seed snapshot. Use only trusted input matching the source and
native tool: CI transfers it between jobs in the same workflow after the macOS
gate, not from an arbitrary other build. The active frontend remains Loom-only.

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

Every invocation still loads and checks source, including contracts and selected
dependency snapshots. Only the single object for the complete checked closure
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
not a promise that tiny programs build faster. Incremental frontend/proof reuse
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
- Immutable records and tagged enums, flat exhaustive `match`, generic type and
  function parameters with inference or explicit arguments. Generic bodies are
  checked without hidden requirements; reachable instances use concrete layouts.
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
  `std.file.read_text` reuses the fallible decoder. Binary `read_bytes/write_bytes` preserve
  arbitrary binary contents and use the same source-owned read/write loops
  and explicit closure as text I/O. Both write APIs create or truncate a file;
  neither promises atomic publication or crash durability.
- Source `std.hash.sha256.digest(Bytes) Bytes` produces a fresh 32-byte digest;
  `hex(Bytes) Text` hashes input and returns its 64 lowercase hexadecimal digits.
  The one-shot implementation uses fixed scratch storage and virtual padding,
  leaves the input unchanged, and also works at compile time within evaluator
  limits. Inputs must be shorter than 2^61 bytes. No hashing runtime operation
  is added; the library is neither a password hash nor an authentication scheme.
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

The [tuples example](examples/tuples/main.loom) covers heterogeneous results,
exactly-once evaluation, and shared fields:

```loom
fn pair[T](value T) (T, Text) { (value, "item") }
let number, label = pair(3)
let single = (true,)
assert single.0
```

Tuple elements and the destructuring initializer evaluate once, left to right.
Tuple types are structural: element types and arity determine identity.
Existing tuple values require matching element types; aggregate widening is not
implicit. Contextual tuple literals can use the existing scalar weakening rules.
`(value)` is grouping, `(value,)` a singleton tuple; `()` remains unavailable.
Numeric projections are checked at compilation. Destructuring currently binds
two or more plain names with exact arity; nested patterns, wildcards, and
parallel reassignment are not implemented. Copying a tuple shares its managed
fields just as copying a record does; compile-time results construct fresh graphs
while preserving internal sharing.

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
allocate-then-fill path to preserve aliases and cycles. Access and mutation use
`std.list.get`, `set`, and `push`; list indexing/repetition syntax is not included.
An element block currently needs a value-producing path: an unconditional
`[{ return }]` is rejected even with a List type context. General typing of
non-returning expressions remains incomplete, as it is for tuples and bindings.

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
in closed executable builds. Method contracts on the concept declaration remain
unsupported. Implementation postconditions still require proofs; an implementation
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
Concept method contracts and extra implementation preconditions currently reject;
implementation postconditions still require proof.

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
record Cache[S Source] { source S value S.Item }
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
conversion supplies evidence. Cross-dyn conversion, concrete recovery, and
compile-time dynamic execution currently reject. A dyn-compatible method can use
bare `Self` only as its first receiver parameter; bound `Self.Item` projections
are allowed elsewhere. Static-only concepts may also use bare `Self` elsewhere.

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
```

The revision must be the full lowercase 40- or 64-digit ID of a commit, not
a tag, branch or abbreviated hash. Replace the illustrative URL/revision above
with a real module repository; its root must contain `loom.toml` with the matching
module name. Sources use credential-free HTTPS URLs with ASCII host/path spelling
and an optional port. URL escapes, query strings, fragments and IPv6 literals
are not supported in this first transport slice. `path` and `git` cannot mix.

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
SHA-256 snapshots. Local path inputs remain editable. Different Git URLs/commits
remain different nominal instances; identical sources reuse one snapshot.
Path dependencies declared inside a Git source cannot escape that snapshot.

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
Git-facing Windows drive/UNC paths use forward slashes without verbatim prefixes;
Loom's canonical paths and native cwd remain unchanged. Windows Git sessions
enable its builtin long-path support, not a promise about arbitrary external tools.
Private-repository authentication, version ranges, graph-wide fork overrides,
source-subdirectory selection and native object caching remain open.
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
Native references use one code pointer and retain only their actual targets,
not every function with the same signature. Managed arguments keep normal GC
protection; scalar callbacks need no Loom runtime.

Pure compile-time calls and returned named function values use the same checked
signatures and contracts. Reification schedules the source declaration and its
type arguments in the destination program, not an evaluation-local function ID.
The [callback example](examples/callbacks/main.loom) uses source `std.list.map`,
overloaded callbacks, returned functions, and GC-stressed argument
ordering. Named compile-time function parameters are also supported; capturing
closures and bound method values remain later work.

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
```

The original boundary condition remains; these constructors add no second check,
even before LLVM optimization. Facts use binding identities, not names. This
slice excludes `var`, heap reads, relationships between different locals, and
facts inferred after branch joins. Literal Float predicates can be reused, but
`!(x > 0.0)` does not prove `x <= 0.0` because of NaN. Unknown cases still return
`Result`; calls and input expressions are never duplicated to seek a proof.

Predicates over `Int` or `Float` may call ordinary pure helpers, including helpers with
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
with immutable locals and a tail expression or return. It uses each checked
specialization in its defining scope and preserves eager arguments, unused
calculations, short-circuit guards and helper preconditions as proof obligations.
For example, replacing `self >= 0` above with `nonnegative(self)` can still remove
the check when `nonnegative` returns `value >= 0`. A helper returning `true` after
`let unused = value + 1` cannot discard a possible overflow. Unsupported helper
control flow, indirect calls or exhausted expansion retain runtime checks.
Required function contracts remain call-free; shared-container constraints,
mutable flow facts and invariant-aware proofs over refined parameters remain open.

`requires` is checked before the callee body. Every declared `ensures` must be
proved; unknown or unsupported proofs reject the build, including for functions
outside the emitted entry closure. There is no runtime postcondition fallback.

The current proof fragment supports scalar linear arithmetic, comparisons,
Boolean facts, local assignments, and acyclic branches/returns. It reasons from
preconditions and successful checked operations. A source-written `assert`
provides a fact only after that assertion succeeds; the compiler never inserts
an assertion to rescue a failed postcondition proof. Postcondition arithmetic
must itself be defined within `Int` bounds.

Calls, loops, nonlinear arithmetic and division in a function requiring proof
are outside this first proof fragment. Contracts themselves are call-free until
the prover supports pure calls. Solver work is bounded; exhaustion is a diagnostic,
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

This slice accepts exact `Int`, `Bool`, `Text`, and function static parameter types.
Literals, pure computed expressions and forwarded static parameters specialize
the declaration; static values participate in instance and computation keys.
Only ordinary parameters remain in the native ABI, with their original relative
evaluation order. A runtime value is an error, not an implicit runtime overload
or fallback. Ordinary surrounding `let` bindings are not automatically promoted
to static bindings.

Function parameters accept named references (including contextually selected
overloads and generics), forwarded static references, and pure computed
selectors. Instance keys contain the source function and its concrete type
arguments; calls to a static target are direct even before LLVM optimization.
Naming an effectful function does not execute it, but calling it during
compile-time evaluation still rejects. Ordinary runtime callback parameters
remain available when the target is not known during checking.

Selected static branches are checked with abstract type arguments and declared
requirements before concrete emission. A Boolean switch does not supply missing
generic capability evidence. Required postconditions still have to pass abstract
declaration checking, even on unused functions; unsupported parameter-dependent
proofs reject. Specialization and pure evaluation remain bounded.

The [static-parameter example](examples/comptime_parameters/main.loom) covers
generic recursion, pure argument computation, static Text, shadowing, returned
ordinary and static callbacks, shared results and runtime argument order. Static
parameters are not yet supported on intrinsics or concept/implementation methods. Taking a reference
to a declaration with static parameters also rejects until explicit partial
specialization can supply a complete function identity. Capturing closures,
variadics and general type-valued computation remain later work.

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
normally. I/O and other external inputs are disallowed. Calls and loops are
bounded by work, depth, and allocation limits; faults or exhausted limits are
diagnostics, never a fallback to runtime execution.
Result expansion is bounded too. Successful pure computations may be reused
within one check after type/capture validation, but each use still reconstructs
fresh runtime containers. This is not a persistent or incremental build cache.

Every branch must parse, but unselected `comptime if` branches impose no type or
call requirements. Type guards compare unshadowed types with `==` or `!=`,
including generic/nested forms such as `T == List[Int]` and
`List[Box[T]] == List[Box[Int]]`; see the
[composite-guard example](examples/comptime/composite.loom). These guards do not
provide general type-valued expressions or Boolean combinations of type
comparisons. An unresolved generic choice requires a contextual result type and
waits for concrete instantiation; code outside that choice is still checked.

The Loom-written evaluator consumes the same checked model as native lowering;
constraint folding and pure-predicate validation use this engine too.
Evaluation is not proof: function `requires`/`ensures` remain call-free and
declared postconditions still require the existing prover. Variadics, typed
macros, and broader compile-time reflection remain later work.
Float compile-time operations and numeric codecs use the same IEEE behavior as
native code, including NaN, infinity, signed zero and subnormals. Float/refined
results may appear inside shared aggregates. Required Float proofs remain
unsupported and reject; successful evaluation is not an algebraic proof.

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
body cannot contain `return`, `?`, or another `defer`, even in an unselected
compile-time branch. Loop control is allowed only for loops inside the cleanup;
it cannot leave the cleanup. Called functions have their own ordinary return scopes.
Pure cleanup also executes during compile-time evaluation. Native lowering uses
direct callbacks and stack registrations, not a general runtime executor. Callbacks
read and update the owner's local storage, including moving-GC roots. Programs
with reachable cleanup route faults through a small LIFO drain, including faults
in called functions without local `defer` blocks. Cleanup-free scalar executables
retain their runtime-free path.
Required proofs still inspect the lowered function; unsupported proofs reject.

The [cleanup example](examples/cleanup/main.loom) exercises these exits and GC
snapshots. Fault draining preserves the first diagnostic and runs remaining
callbacks even if a cleanup faults. It terminates the process; it is not exception
unwinding or recovery. OOM, internal runtime corruption, external process signals,
and explicit process termination do not guarantee cleanup. Task cancellation and
`scoped`/`MustScope` remain unimplemented.

`break` exits the nearest enclosing `while` body; `continue` reevaluates that
loop's condition. Both run the defers of scopes they leave, but not defers
outside the loop. A loop condition is outside its own body's control scope.
Labels and values on loop-control statements are not supported. A `comptime`
block evaluates its own loops and cannot jump into a runtime loop; a selected
`comptime if` branch is ordinary code at its insertion point. Loop execution is
supported at compile time, but general loop-invariant proofs remain unsupported.
The [loops example](examples/loops/main.loom) includes same-package unit tests.

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
Transitively nonallocating functions need no frame. Temporary snapshots protect
managed results only across a later possible allocation; immediate local/return
handoffs and nonallocating reads need no temporary root. Pending arguments and
aggregate fields keep independent snapshots, while completed expressions and
mutually exclusive branches reuse same-type slots. Locals remain function-wide
roots; precise local liveness remains future work.
Ordinary locals remain nonescaping SSA candidates; separate shadow slots mirror
their source writes for the collector, including pattern bindings. After a
possible allocation, used locals and pending expression snapshots reload updated
references. An earlier argument retains its own value even if a later argument
reassigns the source variable.
List/Text accesses use checked typed loads/stores, not runtime accessors.
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
No handles escape the file helpers; every recoverable branch closes the
file explicitly. This is not general scoped cleanup or finalization.

The N0 source-to-native gate is exercised by the examples and integration tests.
N1 now includes the complete source frontend for this subset and native staged
bootstrap through the retained LLVM tool. Replaced Rust source-language stages
are absent from the active tree; the pinned historical fallback is not an
old-language support policy. Stage numbers denote bootstrap generations, not
language versions. The bootstrap subset limits how the compiler source is
written, not what language features the resulting compiler can offer users.
Mutable record fields, broader proofs,
fault-aware/scoped resources, Tasks, complete compile-time programming,
version normalization, authenticated Git sources, graph-wide fork policies,
persistent frontend/proof reuse, deployment and semantic-change tools remain
outside this slice. Exact HTTPS Git/fork resolution, verified source locks and
trusted-local object reuse are implemented. No complete language or `std` claim is made.

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
