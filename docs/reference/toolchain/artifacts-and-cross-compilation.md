# Artifacts and cross-compilation

The word “artifact” covers several outputs with different portability and
compatibility properties. Choose the artifact that matches the next consumer
rather than assuming every build output is executable.

## Artifact kinds

| Output | Produced by | Portable across hosts? | Executable directly? |
| --- | --- | --- | --- |
| Native executable | LLVM `build` | no | yes, on its target platform |
| Relocatable object | LLVM `build --emit object` | target-specific | no |
| `.loomi` | Interpreter `build` | yes within its validated format/language version | via `loomc run --artifact` |
| Portable library (`.loomlib` convention) | `build` of a `lib` target | yes within its validated format/language version | no |
| Runtime bundle | `runtime export` or a release archive | target-specific | used by the linker |

The interpreted artifact format is `loom.interpreted-mir`, currently version
`17`. Portable libraries have format version `1`. The compiler does not append
the `.loomlib` extension automatically. Both formats also record the Loom
language version and are fully decoded and MIR-validated before use.

These are internal toolchain formats, not long-term archival formats. Preserve
the compiler version needed to reproduce important artifacts.

## Host-native builds

With no `--target-triple`, LLVM creates a host target machine using the actual
host CPU and feature set. The emitted PIC object is linked with the runtime
embedded in the same compiler build:

```sh
loomc build --release --target app --output target/app .
```

The native runtime ABI is checked inside the toolchain, but no stable public
native ABI is promised.

## Relocatable cross-target objects

Supplying any `--target-triple` selects a portable target-machine policy:
generic CPU, no target-specific CPU features, PIC relocation.

```sh
loomc build --emit object \
  --target-triple aarch64-unknown-linux-gnu \
  --output target/app.o .
```

This succeeds only when the linked LLVM installation provides the requested
target and its data layout uses 64-bit pointers. Object emission does not prove
that a target operating system, C runtime, system linker, or Loom runtime
bundle is available.

## Cross-target executables

A non-host executable build requires:

1. an LLVM target triple;
2. a runtime bundle built for exactly that target and data layout;
3. an explicit linker capable of linking that target.

```sh
loomc --runtime-bundle /opt/loom/runtime \
  --linker aarch64-linux-gnu-clang \
  build --target-triple aarch64-unknown-linux-gnu \
  --output target/app .
```

The bundle manifest schema is `2`. It binds the target triple, LLVM data
layout, generic CPU policy, runtime ABI identity, archive path and SHA-256, and
required link arguments. The compiler validates the directory tree, manifest,
archive digest, target, ABI, and linker input both before and around linking.
The bundle must be a real directory containing bounded regular files; symlinks
and extra entries are rejected.

`loomc runtime export --output DIR` exports a bundle for the compiler's host
target. The destination must not already exist. A host export does not create
a runtime for another target.

## Release artifacts

The release workflow currently publishes archives for:

- Linux x86-64;
- macOS arm64.

Each archive includes `loomc`, `loom-lsp`, the project README, and a matching
runtime bundle. It is accompanied by a SHA-256 file, and tagged releases also
publish an aggregate `SHA256SUMS`.

Windows is not currently a release-artifact platform. LLVM recognizing a
Windows triple must not be represented as a tested Windows runtime or
executable toolchain.

For the complete distinction between compiler-layer CI, native runtime, cross
target, and release support, see
[Implementation status](../../project/implementation-status.md).
