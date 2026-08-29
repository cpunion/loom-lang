# Quick start

This tutorial uses the repository's executable semantic fixtures. They are checked,
built, tested, and run in automated validation, so the commands and syntax stay
connected to the current compiler.

Build `loomc` first as described in [Installation](installation.md), then run
the commands below from the repository root. The examples use the release
binary; replace `target/release/loomc` with `target/debug/loomc` if you built the
debug profile.

## Check a constrained-value program

[`examples/constraints-contracts/shop.loom`](../../examples/constraints-contracts/shop.loom)
defines a constrained `Price`, a record invariant, a method postcondition, and
ordinary tests:

```sh
target/release/loomc check examples/constraints-contracts
target/release/loomc test examples/constraints-contracts
target/release/loomc run examples/constraints-contracts
```

`check` stops after parsing, lowering, and type checking. `test` compiles and
runs the selected `test fn` and `test async fn` declarations. `run` selects the
exported entry and, by default, compiles and executes a native LLVM artifact.

Build an artifact separately and run that exact artifact:

```sh
target/release/loomc build --output target/constraints-contracts examples/constraints-contracts
target/release/loomc run --artifact target/constraints-contracts
```

Use the interpreter explicitly when you want a semantic comparison:

```sh
target/release/loomc --backend interpreter test examples/constraints-contracts
target/release/loomc --backend interpreter \
  build --output target/constraints-contracts.loomi examples/constraints-contracts
target/release/loomc --backend interpreter \
  run --artifact target/constraints-contracts.loomi
```

Backend selection is a global option and therefore appears before the command.

## Exercise concepts and dynamic dispatch

[`examples/concepts-polymorphism/concepts.loom`](../../examples/concepts-polymorphism/concepts.loom)
covers explicit concept conformance, associated types, static dispatch, and
stored `dyn` values:

```sh
target/release/loomc check examples/concepts-polymorphism
target/release/loomc test examples/concepts-polymorphism
target/release/loomc run examples/concepts-polymorphism
```

See [Concepts and polymorphism](../guide/concepts-and-polymorphism.md) before
using an erased concept value in an API.

## Exercise cleanup and asynchronous tasks

[`examples/async-resources/tasks.loom`](../../examples/async-resources/tasks.loom) covers lexical
`scoped` and `defer` cleanup, suffix `.await`, tuple and list joins, and task
outcomes:

```sh
target/release/loomc check examples/async-resources
target/release/loomc test examples/async-resources
target/release/loomc run examples/async-resources
```

Native asynchronous I/O is supported on the tested Linux and macOS hosts. A
complete Windows native gate is configured, but Windows runtime support is not
claimed until successful runner evidence exists.

## Work with a manifest project

The package example has explicit binary and test targets:

```sh
target/release/loomc --locked check examples/packages/application
target/release/loomc --locked build \
  --target app --output target/package-app examples/packages/application
target/release/loomc --locked test \
  --target unit examples/packages/application
target/release/loomc run --artifact target/package-app
```

The committed `loom.lock` makes `--locked` useful in automation. Run
`loomc resolve PATH` after declaring a dependency, and intentionally use
`loomc resolve --update PATH` when you want the resolver to refresh pins.

## Format source

Format a project in place or verify formatting without changing files:

```sh
target/release/loomc fmt examples/constraints-contracts
target/release/loomc fmt --check examples/constraints-contracts
```

Next, read [Project layout](project-layout.md) to create a manifest project and
the [Language tour](../guide/language-tour.md) for the implemented surface.
