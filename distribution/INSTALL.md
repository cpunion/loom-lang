# Install a local Loom toolchain archive

This archive contains `bin/loom`, the adjacent `lib/loom/loom-native` bridge,
`lib/loom/std`, and the native runtime archive. Windows uses `.exe` and
`loom_runtime.lib` names. `BUILD_INFO.json` records the packaging checkout
revision and host platform; `THIRD_PARTY_NOTICES.md` and `licenses/` contain
dependency notices.
The archive is specific to its build OS and CPU architecture.

Extract it into a new directory. For example, on macOS or Linux:

```sh
mkdir -p "$HOME/opt"
tar -xzf loom-toolchain-OS-ARCH.tar.gz -C "$HOME/opt"
"$HOME/opt/loom-toolchain/bin/loom" --version
```

On Windows, `tar -xzf loom-toolchain-Windows-X64.tar.gz -C C:/tools` installs
`C:/tools/loom-toolchain/bin/loom.exe`. The `tar` command is available in recent
Windows versions and in Git Bash. Extract into a fresh parent directory to avoid
merging with an earlier installation. The adjacent `.sha256` file contains the
archive's SHA-256 digest; check it before extracting (`sha256sum -c` on Linux,
`shasum -a 256 -c` on macOS, or `Get-FileHash` in PowerShell).

To compile native programs, install LLVM 22 shared libraries and a compatible
Clang/linker plus the host SDK. The archive does not bundle those host tools.
On macOS, `brew install llvm@22` and set
`LOOM_CC="$(brew --prefix llvm@22)/bin/clang"`. On Ubuntu 24.04, install
`llvm-22-dev`, `clang-22`, and `libpolly-22-dev` from the LLVM 22 apt repository;
set `LOOM_CC=/usr/bin/clang-22`. On Windows x64, use a Visual Studio developer
shell with an LLVM 22 development package and `clang-cl.exe` on `PATH`; set
`LOOM_CC` to its absolute path. The [CI setup](https://github.com/cpunion/loom-lang/blob/main/.github/workflows/ci.yml)
shows the tested host configuration. `LLVM_SYS_221_PREFIX` and Rust are needed
to build the bridge from source, but are not needed by the archive.

From any working directory, run the installed compiler by its path:

```sh
/path/to/loom-toolchain/bin/loom init hello
/path/to/loom-toolchain/bin/loom check hello
/path/to/loom-toolchain/bin/loom run hello
```

Use `bin/loom.exe` in those commands on Windows. The compiler locates its own
standard library and native bridge relative to its executable. VS Code can use
the absolute compiler path with `loom.stdRoot` left empty. This is a local
toolchain archive, with no installer or automatic `PATH` changes. It does not
promise compatibility with older operating systems or a stable checked-artifact
format.
