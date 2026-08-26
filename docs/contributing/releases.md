# Releases

The release workflow builds and verifies platform archives. It is the source
of truth for published binary support; a successful local cross-object build
does not add a release platform.

## Current archive matrix

- Ubuntu 24.04 -> `linux-x86_64`
- macOS 15 -> `macos-arm64`

Each archive contains:

- `loomc`;
- `loom-lsp`;
- the repository README, license, changelog, roadmap, and community policy
  documents;
- the complete English documentation tree under `docs/`;
- a validated host runtime bundle under `runtime/`.

Each archive has a same-name SHA-256 file. Tagged releases also contain an
aggregate `SHA256SUMS`.

## Release gates

For each archive platform, the workflow:

1. installs Rust 1.88.0 and LLVM 19;
2. separately builds `loom-runtime` with the generic CPU policy;
3. builds `loom-cli` and `loom-lsp` in release mode with `--locked`;
4. packs the explicit runtime archive into `runtime/` beside `loomc`;
5. runs the C3 check/build/test/run loop on LLVM and interpreter backends;
6. verifies portable library artifact creation;
7. runs standard-library and runtime-bundle differential tests;
8. runs `loom-quality`;
9. verifies both release tools expose their command boundary;
10. stages the tools and runtime as siblings, then links and executes a smoke
    program through adjacent-bundle discovery;
11. archives and hashes the exact staged files.

A manually dispatched workflow uploads Actions artifacts named with its run ID,
independent of the source branch spelling. A pushed tag that exactly matches
`v` plus the workspace package version additionally creates the GitHub Release
when necessary and uploads both platform archives and checksums. The workflow
refuses to replace an asset with the same name.

## Preparing a release pull request

Before tagging:

- choose the toolchain SemVer and update all intended package/version
  references consistently;
- decide whether the Loom language version or any wire schema changes;
- update compatibility tables and migration notes;
- update the changelog/release notes with user-visible changes and known
  limitations;
- run the full workspace, fixture, quality, fuzz-smoke, and release-relevant
  tests;
- confirm the platform matrix still matches the workflow;
- confirm runtime ABI changes invalidate bundles and object cache identity;
- ensure dependency updates are locked and reviewed.

Language, manifest, lockfile, MIR, library, cache, registry, and runtime-bundle
versions are independent. Do not bump them merely because the Cargo version
changes, and do not omit a required bump because the release is “only” pre-1.0.

## Tag and verification

Create the release tag only from the reviewed commit according to the
repository's maintainer policy. After the workflow succeeds:

1. download every archive and checksum;
2. verify each SHA-256 independently;
3. inspect the archive file list;
4. run `loomc --version` and `loomc --help`;
5. use the included adjacent runtime bundle in a native smoke build;
6. confirm the release page lists only the two produced platforms.

Do not manually add an untested binary to an existing release. Add a platform
only through a reviewed workflow change with the corresponding CI/native
runtime evidence.

## Rollback

If a release is defective, publish clear release notes and direct users to a
previous verified archive. Do not ask users to combine a previous compiler with
the newer runtime bundle. Preserve the defective artifacts and checksums for
audit unless repository security policy requires otherwise.
