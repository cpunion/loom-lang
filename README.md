# Loom

[![Compiler CI](https://github.com/cpunion/loom-lang/actions/workflows/ci.yml/badge.svg)](https://github.com/cpunion/loom-lang/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Loom is an experimental, statically typed programming language with constrained
values, executable contracts, explicit concepts, automatic memory management,
structured asynchronous tasks, and an LLVM ahead-of-time compiler.

The project explores whether these features can work together in a familiar
text-and-Git workflow without adding ownership, borrowing, or lifetime syntax.

> [!WARNING]
> Loom is a research implementation, not a production-ready language. The
> source language, standard library, diagnostics, artifacts, and runtime ABI
> may change without compatibility guarantees.

## Language at a glance

This excerpt is adapted from the checked and executed
[`examples/core01`](examples/core01/shop.loom) fixture:

```loom
module example.shop

import standard.float.is_finite

pub type Price = Float where is_finite(self) && self >= 0.0

pub record Order {
    subtotal Price
    discount Price

    invariant self.discount <= self.subtotal
}

impl Order {
    pub method total(self) Float
    ensures result >= 0.0
    {
        self.subtotal - self.discount
    }
}

pub fn main() Unit {
    let subtotal = Price(100.0)
    let discount = Price(20.0)
    let order = Order {
        subtotal = subtotal
        discount = discount
    }
    let total = order.total()
    assert total == 80.0
    Unit
}
```

Some important properties are visible in the example:

- parameters and fields use `name Type`, without a separating colon;
- `Price` is a nominal constrained type, not an alias for `Float`;
- record invariants and function contracts are checked in every build profile;
- methods are read-only unless their receiver is written as `mut self`;
- a block's final expression is its result, and semicolons are not used.

Loom also implements closed enums and exhaustive matching, rank-1 generics,
static and erased concept dispatch, moving garbage collection, block-scoped
resource cleanup, stackless coroutines, structured task joins, package
manifests, an LSP server, and native debug information.

## Quick start

Building the toolchain currently requires Rust 1.88.0 and LLVM 19. See the
[installation guide](docs/getting-started/installation.md) for platform-specific
setup.

```sh
git clone https://github.com/cpunion/loom-lang.git
cd loom-lang
cargo +1.88.0 build --locked --release -p loom-cli -p loom-lsp

target/release/loomc check examples/core01
target/release/loomc test examples/core01
target/release/loomc run examples/core01
```

The default backend produces a native executable through LLVM. The interpreter
is an explicitly selected semantic oracle:

```sh
target/release/loomc --backend interpreter test examples/core01
```

Continue with the [quick-start tutorial](docs/getting-started/quick-start.md) or
the [language tour](docs/guide/language-tour.md).

## Toolchain

The workspace provides:

- `loomc` for `check`, `build`, `test`, `run`, `debug`, `fmt`, dependency
  resolution, publishing, runtime-bundle export, and cache inspection;
- `loom-lsp` for diagnostics, navigation, rename, completion, hover, and
  document/workspace symbols;
- an LLVM 19 native backend and an explicit interpreter backend;
- ordinary `.loom` source, `loom.toml` manifests, and `loom.lock` lockfiles;
- portable, versioned `.loomlib` checked-MIR libraries. These are not stable
  native or FFI libraries.

## Platform evidence

The table describes automated evidence, not a stability promise:

| Host | CI coverage | Native release archive |
| --- | --- | --- |
| Ubuntu 24.04, x86-64 | Full workspace, LLVM/interpreter closure, packages, runtime, and quality gates | Yes |
| macOS 15, arm64 | Full workspace, LLVM/interpreter closure, packages, and runtime gates | Yes |
| Windows Server 2025, x86-64 | Platform-independent compiler layers only | No |

Windows LLVM code generation, native linking, runtime I/O, and debugging are
not yet claimed as supported. Cross-target object emission exists for supported
64-bit triples, but producing a cross-target executable requires a matching
Loom runtime bundle and linker.

## Documentation

The [documentation index](docs/README.md) separates getting-started material
from language guides. Useful entry points include:

- [Installation](docs/getting-started/installation.md)
- [Project layout](docs/getting-started/project-layout.md)
- [Constraints and contracts](docs/guide/constraints-and-contracts.md)
- [Concepts and polymorphism](docs/guide/concepts-and-polymorphism.md)
- [Resources and cleanup](docs/guide/resources-and-cleanup.md)
- [Asynchronous programming](docs/guide/asynchronous-programming.md)
- [Packages and dependencies](docs/guide/packages-and-dependencies.md)
- [Roadmap](ROADMAP.md)

## Contributing and security

All changes, including maintainer changes, are made through pull requests. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the development and review workflow.

Please report suspected vulnerabilities privately as described in
[SECURITY.md](SECURITY.md). Do not open a public security issue.

Participation in the project is governed by the
[Code of Conduct](CODE_OF_CONDUCT.md).

See [Support](SUPPORT.md) for help and bug-report routing, and
[Governance](GOVERNANCE.md) for project roles and decisions.

## License

Loom is available under the [MIT License](LICENSE).
