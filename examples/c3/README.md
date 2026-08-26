# Repository-scale package fixture

This committed fixture exercises a multi-package Loom project without generated
sources. It contains three versioned packages and 24 source modules:

- `foundation` defines constrained domain values, records, enums, and a dynamic
  `Named` concept;
- `catalog` is a direct path dependency that implements the concept and composes
  inventory, pricing, shipping, and checkout behavior; and
- `application` contains binary and test targets with explicit direct
  dependencies for every imported package.

From the workspace root, run the native closure:

```sh
cargo +1.88.0 run --locked -p loom-cli -- check --target app examples/c3/application
cargo +1.88.0 run --locked -p loom-cli -- test --target unit examples/c3/application
cargo +1.88.0 run --locked -p loom-cli -- run --target app examples/c3/application
```

Run the same test and entry point through the interpreter for differential
evidence:

```sh
cargo +1.88.0 run --locked -p loom-cli -- \
  --backend interpreter test --target unit examples/c3/application
cargo +1.88.0 run --locked -p loom-cli -- \
  --backend interpreter run --target app examples/c3/application
```

The quality runner records a canonical digest, package and module counts,
checked-MIR size, reachable native roots, and execution timing. Package
isolation, dependency write protection, incremental cache behavior, and backend
differential checks live in deterministic compiler tests rather than mutations
of this fixture.

This is a repository-scale controlled fixture, not evidence from an independent
production project. See [implementation status](../../docs/project/implementation-status.md)
and the [quality policy](../../docs/project/quality-policy.md) for the exact
claims it supports.
