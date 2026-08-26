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

## First build

```sh
cargo +1.88.0 check --locked --workspace --all-targets
cargo +1.88.0 build --locked -p loom-cli -p loom-lsp
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
cargo +1.88.0 test --locked --workspace --all-targets
cargo +1.88.0 build --locked --workspace --all-targets
```

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
