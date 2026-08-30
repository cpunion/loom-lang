# Packages, Git, and registries

Loom resolves one closed module graph before semantic analysis. Module identity
includes its name, resolved SemVer, and Loom language version. Dependencies are
direct and explicitly aliased; resolution does not scan an ambient package
search path.

## Module sources

Loom supports four dependency sources:

1. a local path containing `loom.toml`;
2. a Git repository whose root contains `loom.toml`;
3. a named local or HTTP registry;
4. a validated portable `.loomlib` artifact.

The selected source and, where applicable, its resolved checksum are recorded
in `loom.lock`. A dependency cycle, duplicate module identity, manifest
mismatch, invalid directory package, nested-module boundary violation, or
checksum mismatch stops compilation.

## Git repositories and forks

Declare a repository directly; a fork needs no replacement mechanism:

```toml
[dependencies]
codec = { git = "https://github.com/my-team/codec.git", branch = "loom-fix" }
```

`branch`, `tag`, and the full-commit `rev` selector are mutually exclusive.
With no selector, Loom uses the remote default `HEAD`. `loom.lock` records the
selector and pins the resolved commit and source checksum, so normal builds do
not follow a moving reference. `loom resolve --update` refreshes it
deliberately.

The repository URL is provenance, not nominal type identity. The checked-out
manifest's `[module]` name remains authoritative, while the dependency table
key is the local import alias. Use `module = "NAME"` when those two names differ.

Loom uses the system Git client and its configured HTTPS credential helpers or
SSH agent. It rejects plain HTTP and HTTPS URLs containing credentials, and it
does not echo Git output into diagnostics. Local `file://` repositories are
also accepted. Checkouts live below
`target/loom/git/checkouts`, are published atomically, and are revalidated
against their Git commit and checksum before use. Offline resolution succeeds
only for an already verified checkout.

The initial Git source is deliberately small: `loom.toml` must be at the
repository root, submodules are not initialized, and Git modules cannot use
path or artifact dependencies.

## Local registries

A local registry maps a name to a directory:

```toml
[registries]
company = "../registry"

[dependencies]
utility = { registry = "company", version = "^1" }
```

The registry directory contains `MODULE/VERSION/loom.toml` trees. The resolver
selects the highest SemVer that satisfies the requirement unless a valid
lockfile pin is being reused.

Local registries are filesystem inputs, not HTTP mirrors. They do not use
`publish`.

## HTTP registries

An HTTP registry uses a table:

```toml
[registries.public]
url = "https://registry.example.test"
token-env = "LOOM_REGISTRY_TOKEN"
```

The current protocol schema is `1`:

- `GET /v1/packages/{package}` returns an index of versions and SHA-256 bundle
  digests;
- `GET /v1/packages/{package}/versions/{version}` returns the deterministic
  package bundle;
- `PUT /v1/packages/{package}/versions/{version}` publishes that bundle.

`loom publish --registry public` publishes the root module and refuses a
bundle whose declared identity does not match the manifest. The registry
protocol requires a server to reject replacement bytes for an existing package
version; the current client does not perform a preflight existence check before
`PUT`. On download, the client rejects a changed digest for a version already
present in its validated cache. A lockfile also pins the selected digest.

### Transport and credential rules

Remote registries require HTTPS. Plain HTTP is accepted only for a literal
loopback host, which enables local tests. Credentials embedded in a registry
URL are rejected.

`token-env` names an environment variable whose value is sent as a bearer
token over HTTPS. It cannot be used with plain HTTP. Authentication tokens are
never included in source bundles, and HTTP response bodies from authenticated
requests are not echoed into diagnostics. Keep tokens out of manifests,
command lines, source files, and CI logs.

## Download validation

Registry content is treated as untrusted input. Before a package is used, the
resolver checks:

- protocol version and module/version/language identity;
- index digest and downloaded bundle digest;
- the embedded manifest, directory packages, and nested-manifest boundaries;
- bounded file counts and file sizes;
- portable relative paths, with no symlinks or unexpected materialized files.

The HTTP registry cache does not trust a sidecar metadata file by itself. Every
cache hit re-hashes the raw bundle and verifies every materialized module
file. Corrupt or incomplete cache entries are not used as dependencies.

`--offline` does not weaken those checks. It succeeds only when the locked
version is already present as a fully validated cache entry.

## Portable library dependencies

A version 3 `.loomlib` contains a resolved module graph, exact Loom source
text, and canonical public-interface fingerprints. It deliberately contains no
checked MIR, producer-local proof state, or compiler-owned standard-library
implementation. The decoder rejects incompatible versions and languages,
malformed or oversized graphs and sources, non-portable paths, reserved
`std` module or dependency identities, and interfaces that do not match
the embedded source. The loader bounds the artifact before reading it into
memory. Per-module Merkle identities include dependency identities and their
content, so two artifacts may share an identical transitive module while a
lockfile still detects any transitive source change. The consumer then supplies its matching
compiler-distributed standard library and runs the normal parse, type-check,
proof, and lowering pipeline over the complete source graph.

Only the current `.loomlib` format is accepted. A `.loomlib` is portable across
hosts at the language-artifact level, but it is not a native archive, dynamic
library, stable FFI boundary, or plugin format.

See [Manifest and lockfile](manifest-and-lockfile.md) for dependency syntax and
[Artifacts and cross-compilation](artifacts-and-cross-compilation.md) for the
artifact matrix.
