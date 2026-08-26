# Toolchain reference

This section documents the behavior that users can rely on when invoking the
Loom toolchain. It covers command selection, package resolution, artifacts,
cross-compilation, and caches. Language syntax and semantics belong in the
language reference; compiler implementation details belong in
[Compiler internals](../../internals/README.md).

Loom currently ships two execution backends:

- `llvm` is the default. It emits a native object and, for a supported host or
  an explicitly supplied runtime bundle, links an executable.
- `interpreter` executes validated Loom MIR and can produce a portable `.loomi`
  artifact. It is primarily a semantic reference and diagnostic backend.

Both backends consume the same parsed, resolved, type-checked, and validated
program. Selecting the interpreter does not bypass static checks.

## Contents

- [Command-line interface](cli.md)
- [Manifest and lockfile](manifest-and-lockfile.md)
- [Packages and registries](packages-and-registries.md)
- [Artifacts and cross-compilation](artifacts-and-cross-compilation.md)
- [Caching](caching.md)

## Compatibility boundary

The source language, manifest, lockfile, portable artifacts, compiler cache,
and native runtime bundle are versioned independently. A version number in one
of those formats does not imply compatibility with another format. See
[Versioning](../../project/versioning.md).

The native runtime ABI and the in-memory value layout are compiler-private.
They are checked when an executable is linked, but they are not a stable FFI or
plugin ABI.

## Platform claims

Do not infer platform support merely because LLVM recognizes a target triple.
The project distinguishes:

- a platform on which CI exercises the complete native toolchain;
- a platform on which selected platform-independent compiler layers are
  CI-tested;
- an LLVM target for which a relocatable object can be emitted;
- a target for which a compatible runtime bundle and linker are available;
- a platform for which a release archive is published.

The current matrix is maintained in
[Implementation status](../../project/implementation-status.md).
