# Packages and registries

Loom resolves one closed package graph before semantic analysis. Package
identity includes the package name, resolved SemVer, and Loom language
version. Dependencies are direct and explicitly aliased; resolution does not
scan an ambient module search path.

## Package sources

Loom supports three dependency sources:

1. a local path containing `loom.toml`;
2. a named local or HTTP registry;
3. a validated portable `.loomlib` artifact.

The selected source and resolved checksum are recorded in `loom.lock`. A
dependency cycle, duplicate package identity, manifest mismatch, source-root
escape, or checksum mismatch stops compilation.

## Local registries

A local registry maps a name to a directory:

```toml
[registries]
company = "../registry"

[dependencies]
utility = { registry = "company", version = "^1" }
```

The registry directory contains `PACKAGE/VERSION/loom.toml` trees. The resolver
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

`loomc publish --registry public` publishes the root package and refuses a
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

- protocol version and package/version/language identity;
- index digest and downloaded bundle digest;
- the embedded manifest and its source roots;
- bounded file counts and file sizes;
- portable relative paths, with no symlinks or unexpected materialized files.

The HTTP registry cache does not trust a sidecar metadata file by itself. Every
cache hit re-hashes the raw bundle and verifies every materialized package
file. Corrupt or incomplete cache entries are not used as packages.

`--offline` does not weaken those checks. It succeeds only when the locked
version is already present as a fully validated cache entry.

## Portable library dependencies

A version 2 `.loomlib` contains a resolved package graph, exact Loom source
text, and canonical public-interface fingerprints. It deliberately contains no
checked MIR, producer-local proof state, or compiler-owned standard-library
implementation. The decoder rejects incompatible versions and languages,
malformed or oversized graphs and sources, non-portable paths, reserved
`std` package or dependency identities, and interfaces that do not match
the embedded source. The loader bounds the artifact before reading it into
memory. Per-package Merkle identities include dependency identities and their
content, so two artifacts may share an identical transitive package while a
lockfile still detects any transitive source change. The consumer then supplies its matching
compiler-distributed standard library and runs the normal parse, type-check,
proof, and lowering pipeline over the complete source graph.

Only the current `.loomlib` format is accepted. A `.loomlib` is portable across
hosts at the language-artifact level, but it is not a native archive, dynamic
library, stable FFI boundary, or plugin format.

See [Manifest and lockfile](manifest-and-lockfile.md) for dependency syntax and
[Artifacts and cross-compilation](artifacts-and-cross-compilation.md) for the
artifact matrix.
