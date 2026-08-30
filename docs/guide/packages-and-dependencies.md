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
fork = { git = "https://github.com/example/local-fork.git", branch = "loom-fix" }
published = { registry = "primary", version = "^2" }
prebuilt = { artifact = "../prebuilt.loomlib", version = "^0.4" }
```

- `path` resolves another `loom.toml` relative to the current manifest.
- `git` resolves the module at a Git repository root and pins its exact commit.
- `registry` resolves a module from a named filesystem or HTTPS registry.
- `artifact` consumes a validated portable `.loomlib` without a separate
  producer checkout.

The dependency table key is its local alias and therefore the first segment
used to import that dependency's packages. Use `module = "upstream_name"` when
the dependency's declared module name differs from that alias. The declared name, version
requirement, language version, and resolved module identity must agree.

An artifact dependency cannot combine with `path` or `registry`, and cannot
request source features from the packaged graph. A version 3 `.loomlib`
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

Packages have no implicit initialization hook. Put application startup in
ordinary functions called explicitly from `main`. Top-level `const` values are
fully evaluated and substituted by the compiler; they do not create package
storage or an initialization order. Lazy process state belongs in future `std`
abstractions such as `Lazy` and `Once`. Fixed GC, executor, and Runtime ABI
setup performed by the toolchain remains internal and does not make an import
execute user code.

Dependency `*_test.loom` files are not part of the resolved source graph.
`loom test .` runs the current directory package, while `loom test ./...` runs
every package below the selected root module.

## Git and fork dependencies

A fork is an ordinary dependency source; Loom has no `replace` table:

```toml
[dependencies]
http = { git = "https://github.com/my-team/http.git", branch = "loom-fix" }
```

The checkout's `[module]` declaration remains the nominal identity. Changing
the repository URL to a fork does not create a new type identity, and the table
key remains only the local import prefix. If the key differs from the declared
identity, write it explicitly:

```toml
http_fork = { git = "ssh://git@github.com/my-team/http.git", tag = "v1.4.0", module = "http" }
```

A Git dependency accepts at most one selector:

- `branch = "NAME"` follows a branch when pins are refreshed;
- `tag = "NAME"` selects a tag;
- `rev = "40_HEX_DIGIT_COMMIT"` selects one full commit ID;
- no selector follows the repository's default `HEAD`.

Normal resolution reuses the exact commit in `loom.lock`. Only
`loom resolve --update` refreshes a moving branch, tag, or default `HEAD`.
The lock record carries the selector, exact commit, and a source checksum.
Changing the selector makes `--locked` fail even when two selectors currently
point at the same commit. Every cached checkout is verified against both Git
and that checksum before compilation. `--offline` requires that verified
checkout to exist already.

Loom invokes the system `git` executable so HTTPS credential helpers and SSH
configuration work without manifest secrets. Plain HTTP and credential-bearing
HTTPS URLs are rejected. `file://` is accepted for local repository workflows;
prefer `path` when no Git commit boundary is needed. Keep credentials out of
manifests and diagnostics.

The first Git implementation expects `loom.toml` at the repository root and
does not initialize submodules. A Git-sourced module cannot use path or artifact
dependencies; its transitive dependencies must be Git or registry sources.

## Resolve and lock

Resolve the graph and materialize `loom.lock`:

```sh
loom resolve .
```

An existing lockfile keeps compatible pins. Refresh them intentionally with:

```sh
loom resolve --update .
```

Use the global `--locked` option in automation:

```sh
loom --locked check .
loom --locked test .
```

It fails if the selected module and feature graph does not exactly match the
lockfile. Registry, artifact, and Git content is hashed; changing the contents
of an already locked source is rejected.

`--offline` prohibits registry and Git network requests and accepts only fully
validated local cache hits:

```sh
loom --offline --locked check .
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
loom --features binary_codec check .
loom --no-default-features check .
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
loom publish --registry primary .
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
loom build --target app --output target/app .
loom test .
loom test ./...
loom build --target api --output target/api.loomlib .
```

Tests are selected by the command and `_test.loom` suffix, not by a manifest
target. `loom test` therefore has no `--target` option. `run` rejects library
targets; a library has no executable entry. Native build artifacts and
compiler-private runtime interfaces are not promised to remain compatible
across Loom versions.

## Compiler cache

The default project cache is `target/loom/cache/v12`. It caches versioned
parsing, interfaces, validated checked MIR, target objects, and supported
portable final artifacts using content-derived keys. Reads validate envelopes
and hashes; corrupt or incompatible entries degrade to misses rather than being
trusted.

Use:

```sh
loom cache stat .
loom cache prune .
loom --no-cache check .
loom --cache-dir /absolute/cache/path check .
```

Cache keys include the inputs relevant to their layer, including module and
feature graphs, compiler identity, language version, and target/codegen identity
where applicable. A cache is an optimization, never the authority for source,
lockfile, Git, artifact, or registry integrity.
