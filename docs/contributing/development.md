# Development setup

Loom is a Rust 2024 workspace with a pinned minimum Rust version of 1.88 and a
native backend built against LLVM 19. Use a pull request for repository
changes; keep each change focused enough that its tests and documentation can
be reviewed together.

## Prerequisites

- Git
- Rust 1.88.0 with `rustfmt` and `clippy`
- LLVM 19 development files
- Clang 19 or another compatible host linker

Install the Rust toolchain:

```sh
rustup toolchain install 1.88.0 --component rustfmt --component clippy
```

### Ubuntu 24.04

```sh
sudo apt-get update
sudo apt-get install clang-19 llvm-19-dev libpolly-19-dev
export LLVM_SYS_191_PREFIX=/usr/lib/llvm-19
export LOOM_CC=clang-19
```

### macOS

```sh
brew install llvm@19
export LLVM_SYS_191_PREFIX="$(brew --prefix llvm@19)"
export LOOM_CC="$LLVM_SYS_191_PREFIX/bin/clang"
export PATH="$LLVM_SYS_191_PREFIX/bin:$PATH"
"$LLVM_SYS_191_PREFIX/bin/llvm-config" --version
```

The reported LLVM version must begin with `19.`.

A complete Windows native job is configured for the workspace, `loomc`, LLVM
codegen, the runtime, and native fixture closures. Until that job produces a
real Windows runner result, this is configuration rather than verified platform
or release evidence.

Both Windows CI and release jobs use
`.github/scripts/bootstrap-windows-llvm.ps1`. The pinned LLVM 19.1.7 Windows
development archive contains `LLVM-C.dll` and its `LLVM-C.lib` import library.
MSVC builds use that supported C ABI rather than LLVM's static component
closure. This keeps LLVM's C++ and C-runtime implementation behind one DLL
boundary and removes the archive's omitted static `libxml2s.lib` dependency.
The bootstrap verifies both DLL files, and Loom's object-build fingerprint
includes their exact bytes, so replacing either one invalidates native object
cache entries.

Loom also compiles target initialization from the exact
`llvm-config --targets-built` set. A complete LLVM installation keeps the
all-target initializer for explicit cross-target requests. Native object
emission uses LLVM's native-target entry point. A release package containing
only a subset, such as LLVM's Windows archive, references only the target
libraries it actually ships and initializes only the architecture selected by
an explicit target triple. Inkwell's default all-target feature is disabled;
its AArch64, ARM, and X86 initializers are the bounded partial-package set used
by Loom's supported hosts and cross-object tests. This prevents an otherwise
valid host compiler from acquiring unresolved symbols for omitted cross
targets, and it keeps unrelated packaged backends out of native emission.

The LLVM 19 MSVC C ABI is not asked to discover host CPU strings. Windows host
codegen uses the generic x86-64 baseline with no extra features, which is also
the required runtime-archive policy. Linux and macOS retain native host tuning.

Windows `runtime pack` also avoids creating an LLVM target machine solely to
recover the compiler's immutable host identity. The x86-64 MSVC target triple
and LLVM 19 data layout are embedded as compiler-build invariants, with a
cross-target regression test checking the layout against the pinned LLVM.

The Windows native gate executes target-machine construction and the first
LCIR object emission as separate single-test processes before the workspace
suite. `LOOM_LLVM_TRACE_STAGES=1` prints only compiler-owned LLVM boundary
names, never source text, paths, environment values, or host details. The
preceding linkage probes independently cover version reporting, LLVM-owned
messages, target-triple normalization, and target-machine construction.

## First build

```sh
cargo +1.88.0 check --locked --workspace --all-targets
CARGO_ENCODED_RUSTFLAGS='-Ctarget-cpu=generic' \
  cargo +1.88.0 build --locked -p loom-runtime
cargo +1.88.0 build --locked -p loom-cli -p loom-lsp
target/debug/loomc runtime pack \
  --archive target/debug/libloom_runtime.a \
  --output target/debug/runtime
cargo +1.88.0 run --locked -p loom-cli -- --help
```

If `llvm-sys` cannot find LLVM, check `LLVM_SYS_191_PREFIX` and run the
directory's `bin/llvm-config --version`. Do not work around a version mismatch
by pointing the build at another LLVM major release.

## Running an example

```sh
cargo +1.88.0 run --locked -p loom-cli -- \
  --no-cache check examples/core01
cargo +1.88.0 run --locked -p loom-cli -- \
  --no-cache build --output target/core01 examples/core01
target/core01

cargo +1.88.0 run --locked -p loom-cli -- \
  --backend interpreter --no-cache test examples/core01
```

For a manifest target:

```sh
cargo +1.88.0 run --locked -p loom-cli -- \
  --no-cache run --target app examples/c3/application
```

## Workspace map

Start changes at the narrowest responsible crate:

- syntax or formatter: `loom-syntax` and `loom-cli`;
- name/type/contract/concept behavior: `loom-hir`, `loom-sema`;
- executable operation: `loom-mir` validation, `loom-lowering`, interpreter,
  LLVM backend;
- memory/Task behavior: `loom-runtime-abi`, `loom-runtime`, LLVM emitter;
- projects, registries, cache: `loom-driver` and `loom-cli`;
- editor behavior: `loom-driver` snapshots and `loom-lsp`.

Do not implement source semantics only in one backend. A new MIR operation is
incomplete until validation, interpreter semantics, LLVM lowering, artifact
round-trip, cache identity, and tests agree.

## Local checks before a pull request

```sh
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 check --locked --workspace --all-targets
cargo +1.88.0 clippy --locked --workspace --all-targets -- -D warnings
CARGO_ENCODED_RUSTFLAGS='-Ctarget-cpu=generic' \
  cargo +1.88.0 build --locked -p loom-runtime
cargo +1.88.0 build --locked -p loom-cli
runtime_bundle_root="$(mktemp -d)/runtime"
target/debug/loomc runtime pack \
  --archive target/debug/libloom_runtime.a \
  --output "$runtime_bundle_root"
export LOOM_RUNTIME_BUNDLE="$runtime_bundle_root"
cargo +1.88.0 test --locked --workspace --all-targets
cargo +1.88.0 build --locked --workspace --all-targets
```

The separate runtime build is intentional: compiling `loom-codegen-llvm` never
starts Cargo recursively and never embeds a machine-local runtime archive.
Native integration tests require the explicit host archive above; syntax,
type-checking, object emission, and interpreter tests do not.

The fuzz workspace is separate and uses a pinned nightly; see
[Fuzzing](fuzzing.md). Performance work has a separate controlled runner; see
[Benchmarking](benchmarking.md).

## Generated and local files

Keep `target/`, fuzz artifacts, benchmark reports, and local runtime bundles out
of commits unless a test fixture explicitly requires one. Commit deterministic
source fixtures and small malformed wire inputs only when they are necessary
regressions.

Never commit registry tokens, authenticated HTTP response bodies, absolute
developer paths, or cache contents.
