# Packages and dependencies

A module is a source namespace. A package is a versioned collection of modules
described by `loom.toml`. A target chooses a root policy and artifact kind for
the root package. Loom source does not use the Rust term `crate`.

The executable path-dependency example starts at
[`examples/packages/application/loom.toml`](../../examples/packages/application/loom.toml).

## Manifest format

The current manifest schema is version 1 and the current language version is
`0.3`:

```toml
schema = 1
language = "0.3"

[package]
name = "application"
version = "0.1.0"
sources = ["src"]

[dependencies]
utility = { path = "../utility", version = "^1" }

[[target]]
name = "app"
kind = "bin"
entry = "application.start"

[[target]]
name = "unit"
kind = "test"
```

`sources` defaults to `["src"]`. Write `language` explicitly even though the
current parser supplies `0.3` when it is absent; language version is part of
package identity, lockfiles, compiler caches, and portable artifacts.

Manifest parsing rejects unknown fields, invalid Semantic Versions, source
roots that escape the package, duplicate targets, invalid target shapes, and
dependency cycles.

## Dependency sources

A dependency declares exactly one source:

```toml
[dependencies]
local = { path = "../local", version = "^1" }
published = { registry = "primary", version = "^2" }
prebuilt = { artifact = "../prebuilt.loomlib", version = "^0.4" }
```

- `path` resolves another `loom.toml` relative to the current manifest.
- `registry` resolves a package from a named filesystem or HTTPS registry.
- `artifact` consumes a validated portable `.loomlib` without producer source.

The dependency table key is its local alias. Add `package = "real-name"` when
the registry or path package name differs from that alias. The declared name,
version requirement, language version, and resolved package identity must agree.

An artifact dependency cannot combine with `path` or `registry`, and cannot
request source features from already-built code. `.loomlib` contains versioned,
validated checked MIR and public interfaces; it is not a stable native, dynamic
library, plugin, or FFI ABI.

Dependencies only make package modules available. Source still imports names
explicitly:

```loom
import utility.math.increment
```

There is no wildcard import, implicit prelude contribution from dependencies,
or runtime implementation activation.

## Resolve and lock

Resolve the graph and materialize `loom.lock`:

```sh
loomc resolve .
```

An existing lockfile keeps compatible pins. Refresh them intentionally with:

```sh
loomc resolve --update .
```

Use the global `--locked` option in automation:

```sh
loomc --locked check .
loomc --locked test .
```

It fails if the selected package/feature graph does not exactly match the
lockfile. Registry package content is hashed; changing the contents of an
already locked version is rejected.

`--offline` prohibits registry network requests and accepts only a fully
validated local registry cache hit:

```sh
loomc --offline --locked check .
```

## Optional dependencies and features

Features form named dependency-activation groups:

```toml
[dependencies]
codec = { registry = "primary", version = "^2", optional = true }

[features]
default = []
binary-codec = ["dep:codec"]
```

Select root features with global CLI options:

```sh
loomc --features binary-codec check .
loomc --no-default-features check .
```

A dependency may request downstream features with `features = ["name"]` and
disable its defaults with `default-features = false`.

The current feature system only closes named feature sets and activates
optional dependencies. It does not conditionally remove source within a
package, add a `cfg` language surface, import modules, register implementations,
or enable AOP-like contributions.

## Registries

Declare a filesystem registry or an HTTPS registry by name:

```toml
[registries]
local = "../registry"
primary = { url = "https://registry.example", token-env = "LOOM_REGISTRY_TOKEN" }
```

A filesystem registry uses the layout
`<root>/<package>/<semver>/loom.toml`. Registry resolution selects the highest
version satisfying the requirement unless the lockfile supplies a valid pin.

Network authentication is read only from the environment variable named by
`token-env`. Tokens require HTTPS, redirects are disabled, token values are not
written to lockfiles or caches, and authentication response bodies are not
echoed into diagnostics. Plain HTTP is allowed only for unauthenticated literal
loopback addresses used by local protocol tests.

Publish the current package only to a named network registry:

```sh
loomc publish --registry primary .
```

Published and downloaded bundles are deterministic JSON source bundles with
SHA-256 identity. The resolver validates schema, package/version/language
identity, file paths, sizes, and content digest before materializing source.

The HTTP cache does not trust a sidecar file alone. Every cache hit rehashes the
original bundle, validates the materialized files, and rejects extra files,
special files, or symbolic links. Corruption becomes a miss or a hard offline
failure rather than executable input.

## Targets and artifacts

The root package may declare:

- `kind = "bin"` for an executable entry;
- `kind = "test"` for the package test harness;
- `kind = "lib"` for a portable `.loomlib`.

Select a target explicitly when more than one is applicable:

```sh
loomc build --target app --output target/app .
loomc test --target unit .
loomc build --target api --output target/api.loomlib .
```

`run` and `test` reject library targets; a library has no executable entry.
Native build artifacts and compiler-private runtime interfaces are not promised
to remain compatible across Loom versions.

## Compiler cache

The default project cache is `target/loom/cache/v2`. It caches versioned parsing,
interfaces, validated checked MIR, target objects, and supported portable final
artifacts using content-derived keys. Reads validate envelopes and hashes;
corrupt or incompatible entries degrade to misses rather than being trusted.

Use:

```sh
loomc cache stat .
loomc cache prune .
loomc --no-cache check .
loomc --cache-dir /absolute/cache/path check .
```

Cache keys include the inputs relevant to their layer, including package and
feature graphs, compiler identity, language version, and target/codegen identity
where applicable. A cache is an optimization, never the authority for source,
lockfile, artifact, or registry integrity.
