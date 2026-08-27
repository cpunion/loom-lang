# Versioning and compatibility

Loom versions each boundary according to what it protects. Toolchain, language,
artifact, cache, registry, and runtime versions are deliberately independent.

## Current versions

| Boundary | Current version |
| --- | --- |
| Cargo toolchain packages | `0.1.0` |
| Loom language | `0.3` |
| Manifest schema | `1` |
| Lockfile schema | `1` |
| Registry protocol/bundle | `1` |
| Interpreted MIR artifact | format `loom.interpreted-mir`, version `19` |
| Portable library artifact | version `1` |
| Persistent compiler cache | schema `2` |
| LCIR textual dump | version `5` |
| LCIR artifact identity | schema `6` |
| LCIR native-object domain | `loom-lcir-native-object-v2` |
| Legacy native-object domain | `loom-legacy-native-object-v5` |
| LLVM object-cache domain | `loom-llvm-object-cache-v7` |
| Runtime bundle manifest | schema `2` |
| Native runtime ABI component | `8` |
| Coroutine/Task ABI component | `2` |
| Wait ABI component | `1` |
| Standard-library ABI component | `4` |

The exact compiler-private native ABI identity contains additional layout,
text, shadow-stack, witness, list, and runtime component versions. Runtime
bundles compare the whole identity, not only the numeric runtime component.

## Toolchain releases

The repository uses SemVer-shaped Cargo versions. While the toolchain is below
`1.0.0`, minor releases may contain source or artifact incompatibilities.
Release notes must call out:

- language behavior and diagnostic changes;
- manifest, lockfile, registry, artifact, or cache version changes;
- native runtime ABI changes;
- supported/removed CI and release platforms;
- migration or regeneration steps.

A Git tag does not by itself create support for a platform; only archives
successfully produced and checked by the release workflow are release
artifacts.

## Source language

`language = "0.3"` selects the one source language accepted by the current
compiler. Unknown or older language versions are rejected; the compiler does
not silently reinterpret them as the current language.

The manifest currently defaults an omitted `language` to the current version.
New manifests should write the value explicitly so a future compiler cannot
silently change project intent.

A future language version needs explicit compatibility or migration rules. It
must not be inferred from the toolchain package version.

## Format mismatch behavior

| Boundary | Incompatible input behavior |
| --- | --- |
| Manifest or lockfile | Reject with a configuration/version diagnostic. |
| Registry index or bundle | Reject; never use package bytes under the wrong protocol/language identity. |
| `.loomi` or `.loomlib` | Reject before execution/import, then run complete MIR validation for matching versions. |
| Compiler cache | Treat as a miss; versioned roots prevent accidental reuse. |
| Runtime bundle | Reject before linking on schema, target, data layout, ABI, digest, or tree mismatch. |
| Native executable/object | Target-specific and not promised compatible with another runtime or toolchain. |

Never “fix” incompatibility by editing an envelope version or checksum.
Regenerate the artifact with the intended compiler and review any source or
dependency migration.

MIR version `19` makes process-local construction proofs non-portable. A
matching `.loomi` or nested `.loomlib` payload replays each serialized proof;
the local compiler cache instead rebuilds proof-bearing semantic and MIR layers
from source so a warm build retains the cold build's route and eliminated
checks.

## Reproducibility and rollback

Commit `loom.lock` for reproducible applications. Build with `--locked` and the
same toolchain version used in CI. Preserve release archive checksums and the
toolchain version alongside long-lived artifacts.

To roll back, install a complete previous release archive and its matching
runtime bundle after verifying the published SHA-256. Do not mix `loomc`,
`loom-lsp`, caches, portable artifacts, or runtime bundles from unrelated
toolchain versions unless their versioned decoder explicitly accepts them.

The project currently provides no stable public native library or FFI ABI.
That boundary, if added, will require its own compatibility and deprecation
policy.
