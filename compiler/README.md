# Native compiler seed

This is the first executable slice of the replacement compiler described in the
[roadmap](../ROADMAP.md). It is not the complete N0 milestone or a replacement
for every program supported by the superseded compiler in Git history.

One Rust package connects source syntax, checked functions, and LLVM 19
through Inkwell. It does not depend on the removed compiler crates, interpreter,
universal values, runtime bundle, or executor. Scalar-only programs link only
the host C library for fault reporting; managed programs also link the small
Rust runtime. Ordinary arithmetic and calls lower
directly; LLVM's O2 pipeline promotes local storage and removes unused code.

## Build and try it

Use Rust 1.88, LLVM 19 development libraries, and Clang. macOS is the initial
validation host. From the repository root:

```sh
export LLVM_SYS_191_PREFIX="$(brew --prefix llvm@19)"
export LOOM_CC="$LLVM_SYS_191_PREFIX/bin/clang"
cargo build --locked --workspace

target/debug/loom check compiler/examples/scalar
target/debug/loom build compiler/examples/scalar \
  --output target/scalar --emit-ir target/scalar.ll
target/scalar
target/debug/loom test compiler/examples/scalar
target/debug/loom run compiler/examples/scalar
target/debug/loom test compiler/std/int
LOOM_GC_STRESS=1 target/debug/loom run compiler/examples/source
LOOM_GC_STRESS=1 target/debug/loom test compiler/std/list
```

The root Cargo workspace and lockfile build the maintained compiler. Its binary
is named `loom`; there is no compatibility backend.
Development builds find their source `std` beside this manifest; `LOOM_STD` can
select another standard-library directory. This is not yet a relocatable release
package. `--help` lists the small command surface.

## Implemented subset

- Signed, checked 64-bit `Int`, `Bool`, scalar parameters, calls and recursion.
- `let`, `var`, assignment, final-expression returns, early return, `if`/`else`,
  `while`, `assert`, and explicit `discard`. Boolean operators short-circuit.
- Parameter-type/arity overloads with explicit ambiguity errors.
- Immutable records and tagged enums, flat exhaustive `match`, generic type and
  function parameters with inference or explicit arguments. Generic bodies are
  checked without hidden requirements; reachable instances use concrete layouts.
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
- A source `std.int` package with `minimum` and `maximum`, resolved through
  ordinary imports and calls, not compiler tables of library function names.
- Source `std.text`, `std.list`, `std.result`, and `std.file.read_text`. Private
  intrinsic signatures are checked against the runtime ABI and accepted only
  from the configured standard-library source root. Reading loops, UTF-8
  error policy, and explicit file closure are ordinary Loom source.
- Native executable builds when the selected package has `main`; otherwise,
  an object containing its public functions and their dependencies. Object
  symbols are private seed conventions, not a supported foreign ABI.

The scalar example covers recursion, loops, a pre/postcondition pair, an imported
package, `std`, and both test forms. Native tests exercise overflow, division by
zero, short-circuiting, entry reachability, and test exclusion. Faults report a
brief reason and exit unsuccessfully. File errors use source-defined `Result`.

## Contract boundary

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
purity analysis is available. Solver work is bounded; exhaustion is a diagnostic,
not permission to trust an obligation. These are normal-return guarantees, not
proofs of termination or absence of runtime faults.

## Next boundary

The `compiler/examples/data` package exercises records, enums, generic functions,
and both test forms through the same CLI. Scalar-only records stay native values;
enum storage uses its largest variant payload, not the sum of all variants.

`compiler/examples/source` reads its scanner source from disk and produces a
typed token list. It is a small ASCII scanner, not a complete Loom lexer.

The runtime currently uses single-threaded nonmoving mark/sweep GC. Native
frames register managed locals and expression temporaries across allocation;
transitively nonallocating functions need no root frames. `LOOM_GC_STRESS=1`
collects before every allocation for focused testing. Managed executables find
`libloom_seed_runtime.a` beside the compiler, or at `LOOM_RUNTIME_LIBRARY`.
No handles escape `std.file.read_text`; every recoverable branch closes the
file explicitly. This is not general scoped cleanup or finalization.

N0 still needs refined construction. Recursive declarations, mutable record
fields, broader proofs, moving GC, lexical resources, Tasks, metaprogramming,
dependency resolution, lockfile/cache behavior, deployment and semantic-change
tools remain outside this slice. No complete std or self-hosting claim is made.

Unsupported syntax and manifest features reject explicitly. In particular,
dependency and target declarations are not silently ignored. The accepted
[language foundation](../docs/rfcs/language-foundation.md) remains the goal;
these temporary limits do not redefine it.

For this compiler's local gate:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --workspace
cargo test --locked --workspace
```
