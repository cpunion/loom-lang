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
| Runtime bundle | `runtime pack` or a release archive | target-specific | used by the linker |

The interpreted artifact format is `loom.interpreted-mir`, currently version
`23`. Portable libraries have format version `1`; their nested checked-MIR
payload uses version `23`. The compiler does not append
the `.loomlib` extension automatically. Both formats also record the Loom
language version and are fully decoded and MIR-validated before use.

Version 22 records the source-module provenance and compiler-known identity of
the canonical `standard.resource.MustScope` marker. Decoding rejects missing,
redirected, duplicated, or shape-inconsistent identity metadata before the
program can execute. This is structural validation, not publisher
authentication; artifact provenance still belongs to the distribution layer.

Version 23 requires the canonical six-field compiler-private
`ConstraintError` record and rejects the earlier empty synthetic shape.

Source-local construction proofs are not portable certificates. `.loomi` and
`.loomlib` encode them as mandatory predicate/invariant rechecks. A successful
recheck publishes the original nominal value; a failed one raises the canonical
`ArtifactProofRejected` runtime fault instead of producing a source `Result`.
The disposition cannot by itself bypass the embedded condition. Artifact
authenticity still depends on the trusted registry or distribution channel,
not on the MIR envelope.

These are internal toolchain formats, not long-term archival formats. Preserve
the compiler version needed to reproduce important artifacts.

## Host-native builds

With no `--target-triple`, LLVM creates a host target machine. Linux and macOS
use the actual host CPU and feature set. Windows uses the x86-64 generic CPU
baseline and no extra features, matching the portable runtime archive and
giving compiler caches a deterministic identity without an
environment-dependent host-feature query. The emitted PIC object is linked
only through a validated runtime bundle. The compiler resolves that bundle in
this order:

1. `--runtime-bundle DIR`;
2. `LOOM_RUNTIME_BUNDLE`;
3. `runtime/` beside the resolved `loomc` executable.

Release archives use the third form, so an installed compiler works without a
machine-specific flag:

```sh
loomc build --release --target app --output target/app .
```

The host linker is selected by `--linker PROGRAM`, then `LOOM_CC`, then
`clang`. The runtime manifest and archive are validated before linking. The
native runtime ABI remains compiler-private and has no stable public promise.

## Relocatable cross-target objects

Supplying any `--target-triple` selects a portable target-machine policy:
generic CPU, no target-specific CPU features, PIC relocation.

```sh
loomc build --emit object \
  --target-triple aarch64-unknown-linux-gnu \
  --output target/app.o .
```

This succeeds only when the linked LLVM installation provides and Loom can
initialize the requested target. Complete LLVM installations retain their
all-target initializer for explicit cross-target requests. Partial
installations select exactly one of Loom's bounded AArch64, ARM, and X86
initializers from the normalized triple; a different partial target set is not
currently a supported toolchain. An implicit host build uses LLVM's native
initializer on either kind of installation. The complete legacy value route
and both typed LCIR `Text` representations require 64-bit pointers. A completely
supported typed LCIR artifact that does not require that representation may
emit a matching 32-bit relocatable object, but that is not evidence of a
supported 32-bit Loom runtime or executable toolchain. Object emission does
not prove that a target operating system, C runtime, system linker, or Loom
runtime bundle is available.

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

An implicit host bundle uses the normalized target triple that `loomc` was
built for, such as `aarch64-apple-darwin`. LLVM defaults which embed the current
Darwin point version are not bundle identities. An explicitly requested triple
remains exact and is not rewritten to the implicit host identity.

`loomc runtime pack --archive FILE --output DIR` packages a separately built
host runtime archive. The input must be one bounded regular file rather than a
directory or symlink. Its bytes are copied to the target's canonical archive
name, and the compiler generates and reloads the exact manifest before
publishing the directory. The destination must not already exist.

Packing does not compile a runtime and does not turn a host archive into a
cross-target archive. Build `loom-runtime` for the intended target with the
generic CPU policy before packing it. Object emission, `check`, and the
interpreter never discover or load a native runtime bundle.

On Windows x86-64 MSVC, `runtime pack` uses the compiler target and canonical
LLVM 19 data layout embedded at compiler build time. Packaging therefore does
not initialize a target machine merely to recover immutable host identity.
Linux and macOS continue to derive that identity from their native target
machine. Cross-target object emission always validates the requested LLVM
target independently.

## Release artifacts

The release workflow has verified archive paths for:

- Linux x86-64;
- macOS arm64.

It also has a configured Windows Server 2025 x86-64 matrix entry that produces
a `.zip` rather than a `.tar.gz`. That entry is not yet runner-verified release
evidence and must not be treated as current artifact availability.

Each archive includes `loomc`, `loom-lsp`, the project README, and a matching
runtime bundle. The Windows archive additionally carries the pinned
`LLVM-C.dll` required by its compiler binaries and its LLVM license. Every
archive is accompanied by a SHA-256 file, and tagged releases also publish an
aggregate `SHA256SUMS`.

Windows is not yet a verified release-artifact platform. A configured workflow
entry, or LLVM recognizing a Windows triple, must not be represented as a
tested Windows runtime or executable toolchain until the Windows archive gates
have succeeded.

For the complete distinction between compiler-layer CI, native runtime, cross
target, and release support, see
[Implementation status](../../project/implementation-status.md).
