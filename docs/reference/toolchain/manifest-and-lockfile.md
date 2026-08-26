# Manifest and lockfile

A package project is rooted by `loom.toml`. The manifest declares the package
identity, source roots, direct dependencies, registries, features, and build
targets. `loom.lock` records the fully resolved package graph.

Standalone source files and directories without a manifest are also valid
inputs, but they do not define packages, features, targets, or lockfiles.

## Minimal manifest

```toml
schema = 1
language = "0.3"

[package]
name = "application"
version = "1.0.0"

[dependencies]
utility = { path = "../utility", version = "^1" }

[[target]]
name = "app"
kind = "bin"
entry = "application.main"

[[target]]
name = "unit"
kind = "test"
```

Unknown fields are rejected. Package, dependency-alias, registry, target, and
feature names begin with a lowercase ASCII letter and use lowercase letters,
digits, `_`, or `-`. Package versions are SemVer.

## Top-level fields

| Field | Required | Meaning |
| --- | --- | --- |
| `schema` | yes | Manifest schema. The current value is `1`. |
| `language` | no | Source language version. Omission currently means `"0.3"`; writing it explicitly is recommended. |
| `package` | yes | Package name, version, and source roots. |
| `dependencies` | no | Direct dependency aliases. |
| `registries` | no | Local or HTTP registry definitions. |
| `features` | no | Feature graph and optional-dependency activation. |
| `[[target]]` | no | Named binary, test, or portable-library targets. |

`package.sources` defaults to `["src"]`. Each element is a directory or a
`.loom` file inside the package root. Source discovery is deterministic and
rejects roots that escape the package.

## Dependencies

A dependency selects exactly one source:

```toml
[dependencies]
local = { path = "../local", version = "^1" }
remote = { registry = "public", package = "remote-name", version = ">=1, <2" }
prebuilt = { artifact = "../dist/library.loomlib" }
optional_tool = { path = "../tool", optional = true }
```

The dependency table key is the local alias. `package` can name a differently
named registry package. A dependency may also declare:

- `version`: a SemVer requirement;
- `optional = true`: omit it until a feature activates it;
- `features = ["name"]`: enable features in the dependency;
- `default-features = false`: disable the dependency's default feature.

`path`, `registry`, and `artifact` are mutually exclusive. An artifact
dependency is a validated `.loomlib` rather than a native library.

Each source module in a package must live in that package's module namespace.
Imports are resolved through the root package and its declared direct
dependencies; arbitrary files from the current directory are not searched.

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
name = "unit"
kind = "test"

[[target]]
name = "application"
kind = "lib"
```

The supported target kinds are:

- `bin`: an executable entry; `entry` defaults to `main`;
- `test`: the package graph's `test fn` declarations;
- `lib`: a portable checked-MIR package artifact with no entry.

An `entry` is valid only on a binary target.

## Lockfile behavior

`loomc resolve` resolves the graph and writes a deterministic `loom.lock`.
Source commands also materialize an updated lockfile when ordinary resolution
changes it. Commit the lockfile for applications and other reproducible builds.

The lockfile schema is `1`. Each package record includes its name, version,
language version, source identity, enabled features, and resolved dependency
identities. Registry and artifact records also carry a SHA-256 checksum; path
dependencies do not.

- `--locked` fails if the lockfile is missing or differs from the resolved
  graph. It never silently rewrites the lockfile.
- `resolve --update` ignores existing registry pins and selects the highest
  matching available versions.
- `--offline` permits only local sources and already validated cached registry
  content.

Do not hand-edit package checksums or format-version fields. If a lockfile is
incompatible with the installed compiler, regenerate it with the intended
toolchain after reviewing the dependency change.
