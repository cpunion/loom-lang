# Native compiler

The [Loom-written compiler](loom/README.md) implements package loading, parsing,
binding, type checking, bounded required proofs, and checked program emission.
It builds further compiler stages using one retained Rust LLVM/platform tool.
The [roadmap](../ROADMAP.md) distinguishes this macOS bootstrap from completion
of the accepted language; the Rust seed remains only for the initial stage and
transition validation.

The native tool consumes a checked program, not source that it parses or
type-checks again. It shares the seed's LLVM 19 lowering through Inkwell;
there is no second backend or runtime interpreter. Scalar-only programs link only
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
LOOM_GC_STRESS=1 target/debug/loom test compiler/std/list
target/debug/loom build compiler/loom --output target/loom-stage1
target/loom-stage1 build compiler/loom --output target/loom-stage2
target/loom-stage2 build compiler/loom --output target/loom-stage3
cmp target/loom-stage2 target/loom-stage3
target/loom-stage3 test compiler/loom/checking
target/loom-stage3 test compiler/std/result
target/loom-stage3 run compiler/examples/data
```

The root Cargo workspace builds the initial `loom` seed and `loom-native` tool.
The source compiler defaults to `compiler/std` and `target/debug/loom-native`
relative to the working directory; `--std` and `--native-tool` select explicit
paths. The Rust seed instead finds `std` beside its build-time manifest or via
`LOOM_STD`. These are development commands, not a relocatable release package
or a stable compiler-artifact ABI. `--help` lists each tool's command surface.

## Implemented subset

- Signed, checked 64-bit `Int`, `Bool`, scalar parameters, calls and recursion.
- `let`, `var`, assignment, final-expression returns, early return, `if`/`else`,
  `while`, `assert`, and explicit `discard`. Boolean operators short-circuit.
- Parameter-type/arity overloads with explicit ambiguity errors.
- Immutable records and tagged enums, flat exhaustive `match`, generic type and
  function parameters with inference or explicit arguments. Generic bodies are
  checked without hidden requirements; reachable instances use concrete layouts.
  Payload-free enums support equality within the same nominal type.
  Recursive data through `List` has a finite native layout; direct or mutual
  inline layout cycles reject.
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
- A source `std.int` package with `minimum` and `maximum`, resolved through
  ordinary imports and calls, not compiler tables of library function names.
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
  symbols are private seed conventions, not a supported foreign ABI.

The scalar example covers recursion, loops, a pre/postcondition pair, an imported
package, `std`, and both test forms. Native tests exercise overflow, division by
zero, short-circuiting, entry reachability, and test exclusion. Faults report a
brief reason and exit unsuccessfully. File errors use source-defined `Result`.

## Contract boundary

Scalar constrained types use `type Positive = Int where self > 0`.
`Positive(3)` yields `Positive` directly; a false constant is a diagnostic.
An unproved input evaluates once and returns source
`Result[Positive, ConstraintError]`. Widening `Positive` to `Int` emits no check;
arithmetic returns `Int`, while generic inference retains nominal identity.
`List[Positive]` never widens to `List[Int]`.

This construction fragment constrains `Int` directly, with predicates over
`self` and call-free scalar expressions. Constant arithmetic must be defined
before it can justify check removal. Propagating local/branch facts into
construction, pure function calls in predicates, shared-container constraints,
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
purity analysis is available. Solver work is bounded; exhaustion is a diagnostic,
not permission to trust an obligation. These are normal-return guarantees, not
proofs of termination or absence of runtime faults.

## Next boundary

The `compiler/examples/data` package exercises records, enums, generic functions,
and both test forms through the same CLI. Scalar-only records stay native values;
enum storage uses its largest variant payload, not the sum of all variants.

The [Loom-written compiler](loom/README.md) uses ordinary source packages for
syntax, project loading, binding, checking, proof, and typed artifact emission.
It checks its own sources and builds subsequent native stages. Public parser/AST
and optional semantic-query APIs are a separate
[library acceptance gate](../ROADMAP.md#n2--complete-the-language-and-source-library),
not a promise that the current internal structures are stable public schemas.

The runtime currently uses single-threaded nonmoving mark/sweep GC. Native
frames register managed locals and expression temporaries across allocation;
transitively nonallocating functions need no root frames. `LOOM_GC_STRESS=1`
collects before every allocation for focused testing. Managed executables find
`libloom_seed_runtime.a` beside the compiler, or at `LOOM_RUNTIME_LIBRARY`.
No handles escape the file helpers; every recoverable branch closes the
file explicitly. This is not general scoped cleanup or finalization.

The N0 source-to-native gate is exercised by the examples and integration tests.
N1 now includes the complete source frontend for this subset and native staged
bootstrap through the retained LLVM tool. The replaced Rust source frontend
must retire after transition validation; it is not a parallel product target.
Mutable record fields, broader proofs, moving GC,
lexical resources, Tasks, metaprogramming,
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
cargo build --locked --workspace
cargo test --locked --workspace
```
