# Quick start

This tutorial uses the repository's executable semantic fixtures. They are checked,
built, tested, and run in automated validation, so the commands and syntax stay
connected to the current compiler.

Build `loom` first as described in [Installation](installation.md), then run
the commands below from the repository root. The examples use the release
binary; replace `target/release/loom` with `target/debug/loom` if you built the
debug profile.

## Check a constrained-value program

[`examples/constraints-contracts/shop.loom`](../../examples/constraints-contracts/shop.loom)
defines a constrained `Price`, a record invariant, and a method postcondition.
Its tests live in
[`shop_test.loom`](../../examples/constraints-contracts/shop_test.loom):

```sh
target/release/loom check examples/constraints-contracts
target/release/loom test examples/constraints-contracts
target/release/loom run examples/constraints-contracts
```

`check` stops after parsing, lowering, and type checking. It uses the production
projection, which excludes `*_test.loom` and embedded tests. `test` adds the
selected directory package's test companion and runs its `test fn` and
`test async fn` declarations. Use `loom test ./...` to test every package in a
manifest module. `run` selects the exported entry and, by default, compiles and
executes a native LLVM artifact.

Build an artifact separately and run that exact artifact:

```sh
target/release/loom build --output target/constraints-contracts examples/constraints-contracts
target/release/loom run --artifact target/constraints-contracts
```

Use the interpreter explicitly when you want a semantic comparison:

```sh
target/release/loom --backend interpreter test examples/constraints-contracts
target/release/loom --backend interpreter \
  build --output target/constraints-contracts.loomi examples/constraints-contracts
target/release/loom --backend interpreter \
  run --artifact target/constraints-contracts.loomi
```

Backend selection is a global option and therefore appears before the command.

## Exercise concepts and dynamic dispatch

[`examples/concepts-polymorphism/concepts.loom`](../../examples/concepts-polymorphism/concepts.loom)
covers explicit concept conformance, associated types, static dispatch, and
stored `dyn` values:

```sh
target/release/loom check examples/concepts-polymorphism
target/release/loom test examples/concepts-polymorphism
target/release/loom run examples/concepts-polymorphism
```

See [Concepts and polymorphism](../guide/concepts-and-polymorphism.md) before
using an erased concept value in an API.

## Exercise cleanup and asynchronous tasks

[`examples/async-resources/tasks.loom`](../../examples/async-resources/tasks.loom) covers lexical
`scoped` and `defer` cleanup, suffix `.await`, tuple and list joins, and task
outcomes:

```sh
target/release/loom check examples/async-resources
target/release/loom test examples/async-resources
target/release/loom run examples/async-resources
```

Native asynchronous I/O is supported on the tested Linux and macOS hosts. The
Windows release entry exercises the native closure, but Windows runtime support
is not claimed until successful runner and archive evidence exists.

## Work with a manifest project

The module example has one explicit binary target and directory packages:

```sh
target/release/loom --locked check examples/packages/application
target/release/loom --locked build \
  --target app --output target/package-app examples/packages/application
target/release/loom --locked test examples/packages/application
target/release/loom run --artifact target/package-app
```

The committed `loom.lock` makes `--locked` useful in automation. Run
`loom resolve PATH` after declaring a dependency, and intentionally use
`loom resolve --update PATH` when you want the resolver to refresh pins.

## Format source

Format a project in place or verify formatting without changing files:

```sh
target/release/loom fmt examples/constraints-contracts
target/release/loom fmt --check examples/constraints-contracts
```

Next, read [Project layout](project-layout.md) to create a manifest project and
the [Language tour](../guide/language-tour.md) for the implemented surface.
