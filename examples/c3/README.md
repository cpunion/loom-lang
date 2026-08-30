# Repository-scale package fixture

This committed fixture exercises a three-module Loom project without generated
sources. It contains three versioned modules, 24 directory packages, and 25
source files:

- the `foundation` module defines constrained domain values, records, enums,
  and a dynamic `Named` concept;
- the `catalog` module is a direct path dependency that implements the concept
  and composes inventory, pricing, shipping, and checkout behavior; and
- the `application` module contains a binary target and `*_test.loom` tests,
  with explicit direct module dependencies for its imported packages.

From the workspace root, run the native closure:

```sh
cargo +1.88.0 run --locked -p loom-cli -- check --target app examples/c3/application
cargo +1.88.0 run --locked -p loom-cli -- test examples/c3/application/...
cargo +1.88.0 run --locked -p loom-cli -- run --target app examples/c3/application
```

Run the same test and entry point through the interpreter for differential
evidence:

```sh
cargo +1.88.0 run --locked -p loom-cli -- \
  --backend interpreter test examples/c3/application/...
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
