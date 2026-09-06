# Native compiler

The [Loom-written compiler](loom/README.md) implements package loading, parsing,
binding, type checking, bounded required proofs, and checked program emission.
It builds further compiler stages using one retained Rust LLVM/platform tool.
The [roadmap](../ROADMAP.md) distinguishes the native bootstrap from completion
of the accepted language. A previous Loom compiler is the bootstrap input;
the frozen historical Rust seed is only a fallback for producing that input.

The native tool consumes a checked program, not source that it parses or
type-checks again. It uses LLVM 22 through Inkwell; there is no second language
frontend or runtime interpreter. Scalar-only programs link only
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
target/loom build compiler/examples/arguments --output target/arguments
target/arguments +0010
```

The [bootstrap script](../scripts/bootstrap.sh) builds the current Rust tool
and runtime, then Loom stages 1, 2, and 3. It compares stages 2/3 byte-for-byte
and publishes `target/loom`. On macOS/Linux, a cold build starts with the frozen
Rust seed in `compiler/bootstrap/seed`, then builds the ordered Loom source
commits in `compiler/bootstrap/checkpoints`. Each checkpoint implements a
capability before the next compiler uses it: the first enables native Float
before the current evaluator stores Float values. These immutable inputs are
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
```

The [harness](../scripts/benchmark-compiler.mjs) reports median wall time and
macOS peak RSS for scalar, data, and compiler packages, with raw samples in
`target/performance/compiler.json`. `--compiler`, `--output`, and `--runs`
select the binary, report, and sample count.

Every sample starts a fresh process after one warmup; OS caches are warm.
There is no incremental compiler cache yet. Native decode, codegen, and linker
timings separate backend costs; remaining build wall time also includes
serialization and process/pipe overhead, not just frontend analysis. Peak RSS
is the operating system's reported maximum, not summed concurrent process
memory. Startup and isolated test-compilation measurements remain follow-up
coverage, as does tracking growth on larger packages.

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
- IEEE binary64 `Float`, decimal/exponent literals, arithmetic and comparisons.
  Float division/remainder follow IEEE rules rather than integer faults; NaN,
  infinities and signed zero are retained. No implicit Int/Float conversion or
  fast-math reassociation is permitted. Scalar Float programs need no Loom runtime.
- `let`, `var`, assignment, final-expression returns, early return, `if`/`else`,
  `while`, `assert`, and explicit `discard`. Boolean operators short-circuit.
- Parameter-type/arity overloads with explicit ambiguity errors.
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
- Directory packages, private helpers and `pub`, package-wide explicit imports,
  and a local import closure. A simple `loom.toml` supplies the module name;
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

## Static concepts

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

This slice supports nongeneric concepts and concrete implementation targets.
Associated types, generic/default implementations, concept-typed parameter
shorthand, bounded generic data declarations and `dyn C` remain later work.
Concept method contracts and extra implementation preconditions currently reject;
implementation postconditions still require proof.

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

Propagating local/branch facts into construction, shared-container constraints,
and invariant-aware proofs over refined parameters are not implemented yet.

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

## Next boundary

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

The runtime currently uses single-threaded nonmoving mark/sweep GC. Native
frames register managed locals and expression temporaries across allocation;
transitively nonallocating functions need no root frames.
Temporaries in completed statements and mutually exclusive branches reuse
same-type slots. Pending arguments, enclosing results, and permanent locals keep
their own roots; no runtime registration is added at each statement.
`LOOM_GC_STRESS=1`
collects before every allocation for focused testing. The LLVM tool links managed
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
Mutable record fields, broader proofs, moving GC,
lexical resources, Tasks, complete compile-time programming,
dependency resolution, lockfile/cache behavior, deployment and semantic-change
tools remain outside this slice. No complete language or `std` claim is made.

Unsupported syntax and manifest features reject explicitly. In particular,
dependency and target declarations are not silently ignored. The accepted
[language foundation](../docs/rfcs/language-foundation.md) remains the goal;
these temporary limits do not redefine it.

For this compiler's local gate:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
bash scripts/bootstrap.sh --dev
cargo test --locked --workspace
```
