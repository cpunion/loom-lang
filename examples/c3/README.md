# C3 multi-package workload

This fixture is a committed, non-generated repository workload for the Loom
compiler. It contains three versioned packages and 24 source modules:

- `foundation`: constrained domain values, records, enums, and a dynamic
  `Named` concept;
- `catalog`: a direct path dependency that implements the concept and composes
  inventory, pricing, shipping, and checkout behavior;
- `application`: a binary/test package with explicit direct dependencies on
  every package it imports.

Run the complete closure from the workspace root:

```sh
cargo run -p loom-cli -- check --target app examples/c3/application
cargo run -p loom-cli -- test --target unit examples/c3/application
cargo run -p loom-cli -- run --target app examples/c3/application
cargo run -p loom-cli -- --backend interpreter test --target unit examples/c3/application
cargo run -p loom-cli -- --backend interpreter run --target app examples/c3/application
```

The quality runner records a canonical digest, package/module counts, checked
MIR size, reachable native roots, and execution timings for this repository.
Package-isolation, dependency-write-protection, incremental-cache, and backend
differential regressions are maintained as deterministic compiler tests rather
than by mutating this fixture during CI.
