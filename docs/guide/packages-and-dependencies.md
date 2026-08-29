# Packages and dependencies

A Loom module is a named, versioned dependency unit rooted by `loom.toml`. A
package is one source directory inside that module. This follows the useful
part of Go's organization: directories define packages, while the manifest
defines the versioned module that contains them. Loom does not use the Rust
term `crate`.

The executable path-dependency example starts at
[`examples/packages/application/loom.toml`](../../examples/packages/application/loom.toml).

## Manifest format

The current manifest schema is version 2 and the current language version is
`0.3`:

```toml
schema = 2
language = "0.3"

[module]
name = "application"
version = "0.1.0"

[dependencies]
utility = { path = "../utility", version = "^1" }

[[target]]
name = "app"
kind = "bin"
entry = "application.start"
```

The manifest directory is the module root. Source begins there directly; there
is no special `src/` directory and no `sources` field. A subdirectory containing
`.loom` files forms a package whose path is the module name followed by its
relative directory segments. A nested `loom.toml` starts another module and is
not traversed as part of its parent.

Write `language` explicitly even though the current parser supplies `0.3` when
it is absent. Language version is part of module identity, lockfiles, compiler
caches, and portable artifacts.

Manifest parsing rejects unknown fields, invalid Semantic Versions, invalid
directory package names, duplicate targets, invalid target shapes, dependency
cycles, and source paths that cross a nested module boundary.

## Dependency sources

A dependency declares exactly one source:

```toml
[dependencies]
local = { path = "../local", version = "^1" }
published = { registry = "primary", version = "^2" }
prebuilt = { artifact = "../prebuilt.loomlib", version = "^0.4" }
```

- `path` resolves another `loom.toml` relative to the current manifest.
- `registry` resolves a module from a named filesystem or HTTPS registry.
- `artifact` consumes a validated portable `.loomlib` without a separate
  producer checkout.

The dependency table key is its local alias and therefore the first segment
used to import that dependency's packages. The declared name, version
requirement, language version, and resolved module identity must agree.

An artifact dependency cannot combine with `path` or `registry`, and cannot
request source features from the packaged graph. A version 2 `.loomlib`
contains the resolved module graph, exact Loom sources, and canonical public
interfaces. It contains no checked MIR, producer-local proof state, or copy of
the compiler-distributed standard library. The consumer validates the envelope
and interface fingerprints, supplies its matching standard library, and then
parses, type-checks, proves, and lowers the packaged sources normally. It is not
a stable native or dynamic library, plugin, or FFI ABI.

Dependencies make public package declarations available but never inject names
into source. Import one symbol explicitly:

```loom
import utility.math.increment
```

There are no wildcard imports, dependency-provided preludes, or runtime
implementation activation.

Dependency `*_test.loom` files are not part of the resolved source graph.
`loomc test` includes and runs tests only from the selected root module.

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

It fails if the selected module and feature graph does not exactly match the
lockfile. Registry content is hashed; changing the contents of an already
locked version is rejected.

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
binary_codec = ["dep:codec"]
```

Select root-module features with global CLI options:

```sh
loomc --features binary_codec check .
loomc --no-default-features check .
```

A dependency may request downstream features with `features = ["name"]` and
disable its defaults with `default-features = false`.

The feature system only closes named feature sets and activates optional
dependencies. It does not conditionally remove source within a module, add a
`cfg` language surface, import packages, register implementations, or enable
AOP-like contributions.

## Registries

Declare a filesystem registry or an HTTPS registry by name:

```toml
[registries]
local = "../registry"
primary = { url = "https://registry.example", token-env = "LOOM_REGISTRY_TOKEN" }
```

A filesystem registry uses the layout
`<root>/<module>/<semver>/loom.toml`. Registry resolution selects the highest
version satisfying the requirement unless the lockfile supplies a valid pin.

Network authentication is read only from the environment variable named by
`token-env`. Tokens require HTTPS, redirects are disabled, token values are not
written to lockfiles or caches, and authentication response bodies are not
echoed into diagnostics. Plain HTTP is allowed only for unauthenticated literal
loopback addresses used by local protocol tests.

Publish the current module only to a named network registry:

```sh
loomc publish --registry primary .
```

Published and downloaded bundles are deterministic JSON source bundles with
SHA-256 identity. The resolver validates schema, module identity, version,
language, file paths, sizes, and content digest before materializing source.

The HTTP cache does not trust a sidecar file alone. Every cache hit rehashes the
original bundle, validates the materialized files, and rejects extra files,
special files, or symbolic links. Corruption becomes a miss or a hard offline
failure rather than executable input.

## Targets and artifacts

The root module may declare:

- `kind = "bin"` for an executable entry;
- `kind = "lib"` for a portable `.loomlib`.

Select a build target explicitly when more than one is applicable:

```sh
loomc build --target app --output target/app .
loomc test .
loomc build --target api --output target/api.loomlib .
```

Tests are selected by the command and `_test.loom` suffix, not by a manifest
target. `loomc test` therefore has no `--target` option. `run` rejects library
targets; a library has no executable entry. Native build artifacts and
compiler-private runtime interfaces are not promised to remain compatible
across Loom versions.

## Compiler cache

The default project cache is `target/loom/cache/v6`. It caches versioned
parsing, interfaces, validated checked MIR, target objects, and supported
portable final artifacts using content-derived keys. Reads validate envelopes
and hashes; corrupt or incompatible entries degrade to misses rather than being
trusted.

Use:

```sh
loomc cache stat .
loomc cache prune .
loomc --no-cache check .
loomc --cache-dir /absolute/cache/path check .
```

Cache keys include the inputs relevant to their layer, including module and
feature graphs, compiler identity, language version, and target/codegen identity
where applicable. A cache is an optimization, never the authority for source,
lockfile, artifact, or registry integrity.
