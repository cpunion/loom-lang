# Installation

Loom does not yet publish a stable toolchain. The authoritative way to use the
current implementation is to build it from source with the pinned Rust and LLVM
versions.

## Host support

The following table describes automated evidence, not a compatibility promise:

| Host | Current evidence | Native archive |
| --- | --- | --- |
| Ubuntu 24.04, x86-64 | Full workspace, LLVM and interpreter fixtures, packages, runtime, and quality gates | Yes |
| macOS 15, arm64 | Full workspace, LLVM and interpreter fixtures, packages, and runtime gates | Yes |
| Windows Server 2025, x86-64 | Complete native job configured; successful runner evidence pending | `.zip` workflow entry configured; not yet verified or published |

The Windows jobs cover native code generation, linking, runtime I/O,
CodeView/PDB inspection, and the structure of a release zip, but Windows
support is not claimed until successful runner and archive evidence exists.
Object emission for another target also does not provide that target's runtime
or linker.

## Prerequisites

You need:

- Git;
- Rust 1.88.0, including Cargo and rustfmt;
- LLVM 19 development libraries and Clang 19;
- a native C/C++ system linker supported by Clang.

Install the pinned Rust toolchain with rustup:

```sh
rustup toolchain install 1.88.0 --profile minimal --component rustfmt
```

On Ubuntu 24.04, install the LLVM packages and identify the LLVM installation:

```sh
sudo apt-get update
sudo apt-get install -y clang-19 llvm-19-dev libpolly-19-dev
export LLVM_SYS_191_PREFIX=/usr/lib/llvm-19
export LOOM_CC=clang-19
```

On macOS, install the versioned Homebrew formula and expose its binaries and
libraries to the build:

```sh
brew install llvm@19
export PATH="$(brew --prefix llvm@19)/bin:$PATH"
export LLVM_SYS_191_PREFIX="$(brew --prefix llvm@19)"
export LOOM_CC="$(brew --prefix llvm@19)/bin/clang"
```

Confirm that LLVM 19 is selected before compiling:

```sh
"$LLVM_SYS_191_PREFIX/bin/llvm-config" --version
"$LOOM_CC" --version
```

## Build from source

Clone the repository, build the runtime archive with the portable CPU policy,
build the tools, and pack the runtime beside `loomc`:

```sh
git clone https://github.com/cpunion/loom-lang.git
cd loom-lang
CARGO_ENCODED_RUSTFLAGS='-Ctarget-cpu=generic' \
  cargo +1.88.0 build --locked --release -p loom-runtime
cargo +1.88.0 build --locked --release -p loom-cli -p loom-lsp
target/release/loomc runtime pack \
  --archive target/release/libloom_runtime.a \
  --output target/release/runtime
```

The binaries are written to:

- `target/release/loomc`
- `target/release/loom-lsp`
- `target/release/runtime/loom-runtime-bundle.json` and its runtime archive

Verify the compiler:

```sh
target/release/loomc --version
target/release/loomc check examples/core01
```

Loom has no installer or shell-completion command yet. Keep the `runtime/`
directory beside the resolved `loomc` executable, then add `target/release` to
your `PATH` or invoke the binaries by their explicit paths. If a deployment
stores the bundle elsewhere, set `LOOM_RUNTIME_BUNDLE` or pass
`--runtime-bundle`; the explicit option takes precedence.

## Release archives

Development releases may provide `.tar.gz` archives for Linux x86-64 and macOS
arm64. The workflow is also configured to build a Windows x86-64 `.zip`, but
that entry is not a support or availability claim until a Windows runner has
successfully produced and checked it. Treat the release page and its checksum
file as the source of truth for the artifacts attached to a particular tag.
Verify the downloaded archive before running it; do not assume that an archive
exists merely because its matrix entry is configured.

## Troubleshooting LLVM discovery

If Cargo cannot find LLVM, check that `LLVM_SYS_191_PREFIX` points to the prefix
containing `bin/llvm-config`, `include/llvm`, and the LLVM libraries. Multiple
LLVM installations on the same host are a common cause of link failures.

If Loom builds but cannot link a program, confirm that the adjacent runtime
bundle exists and validates, `LOOM_CC` names Clang 19, and the host's native
linker and system SDK are installed. The interpreter backend can still exercise
language semantics without a native runtime bundle or LLVM linking:

```sh
target/release/loomc --backend interpreter test examples/core01
```

This is a diagnostic alternative, not the default production path.

Continue with the [quick start](quick-start.md).
