# Manifest and lockfile

A Loom module is rooted by `loom.toml`. The manifest declares its versioned
identity, direct dependencies, registries, features, and build targets. Source
packages are derived from directories below that manifest; they are not listed
in the manifest. `loom.lock` records the fully resolved module graph.

Standalone source files and directories without a manifest are also valid
inputs, but they do not define a versioned module, features, targets, or a
lockfile.

## Minimal manifest

```toml
schema = 2
language = "0.3"

[module]
name = "application"
version = "1.0.0"

[dependencies]
utility = { path = "../utility", version = "^1" }

[[target]]
name = "app"
kind = "bin"
entry = "application.main"
```

Unknown fields are rejected. Module, directory-package, dependency-alias,
registry, target, and feature names begin with a lowercase ASCII letter and use
only lowercase letters, digits, or `_`. Module versions are SemVer.

## Top-level fields

| Field | Required | Meaning |
| --- | --- | --- |
| `schema` | yes | Manifest schema. The current value is `2`. |
| `language` | no | Source language version. Omission currently means `"0.3"`; writing it explicitly is recommended. |
| `module` | yes | Module name and version. |
| `dependencies` | no | Direct dependency aliases. |
| `registries` | no | Local or HTTP registry definitions. |
| `features` | no | Feature graph and optional-dependency activation. |
| `[[target]]` | no | Named binary or portable-library targets. |

The manifest directory is always the module root. There is no `sources` field
and `src/` has no special behavior. Source discovery recursively includes
`.loom` files below the module root, except ignored build directories and
subtrees owned by a nested `loom.toml`.

The root directory is the package named by the module. Each relative source
directory extends that package path. For module `application`, files directly
beside the manifest belong to `application`, while files in `http/client/`
belong to `application.http.client`. A file name never changes its package.

Ordinary source selection excludes `*_test.loom`. `loomc test` includes those
files only for the root module. Dependency test files are never selected.

## Dependencies

A dependency selects exactly one source:

```toml
[dependencies]
local = { path = "../local", version = "^1" }
fork = { git = "https://github.com/example/remote_name.git", branch = "loom-fix", module = "remote_name" }
remote = { registry = "public", module = "remote_name", version = ">=1, <2" }
prebuilt = { artifact = "../dist/library.loomlib" }
optional_tool = { path = "../tool", optional = true }
```

The dependency table key is the local alias and import prefix. `module` can
name a differently named dependency module. A dependency may also declare:

- `version`: a SemVer requirement;
- `optional = true`: omit it until a feature activates it;
- `features = ["name"]`: enable features in the dependency;
- `default-features = false`: disable the dependency's default feature.

`path`, `git`, `registry`, and `artifact` are mutually exclusive. A Git
dependency accepts at most one of `branch`, `tag`, or a full 40-hex-digit
`rev`; omitting all three selects the repository's default `HEAD`. An artifact
dependency is a validated `.loomlib` rather than a native library.

Imports resolve through the root module and its declared direct dependency
aliases; the compiler does not search arbitrary files or an ambient package
path. Each import identifies one public declaration by its fully qualified
package path.

## Features

Features form an acyclic named graph:

```toml
[dependencies]
metrics = { path = "../metrics", optional = true }

[features]
default = ["observability"]
observability = ["dep:metrics"]
```

Feature members are either another feature name or `dep:ALIAS` for an optional
dependency. Unknown members, duplicate members, cycles, and attempts to
activate a non-optional dependency are errors.

Features currently affect dependency activation and dependency feature
requests. They are not a conditional-compilation syntax for source
declarations.

## Targets

```toml
[[target]]
name = "service"
kind = "bin"
entry = "application.service"

[[target]]
name = "application"
kind = "lib"
```

The supported target kinds are:

- `bin`: an executable entry; `entry` defaults to `main`;
- `lib`: a portable source-and-interface module artifact with no entry.

An `entry` is valid only on a binary target. Tests are not targets: `loomc test`
selects root-module `*_test.loom` files directly and does not accept `--target`.

## Lockfile behavior

`loomc resolve` resolves the graph and writes a deterministic `loom.lock`.
Source commands also materialize an updated lockfile when ordinary resolution
changes it. Commit the lockfile for applications and other reproducible builds.

The lockfile schema is `2`. Each `[[module]]` record includes its name, version,
language version, source identity, enabled features, and resolved dependency
identities. Git source identity includes the requested selector and exact
commit pin. Registry, Git, and artifact records also carry a SHA-256 checksum;
path dependencies do not.

- `--locked` fails if the lockfile is missing or differs from the resolved
  graph. It never silently rewrites the lockfile.
- `resolve --update` ignores existing registry and Git pins and refreshes their
  selectors.
- `--offline` permits only local sources and already validated cached registry
  or Git content.

Do not hand-edit module checksums or format-version fields. If a lockfile is
incompatible with the installed compiler, regenerate it with the intended
toolchain after reviewing the dependency change.
