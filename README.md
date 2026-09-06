# Loom

Loom is an experimental compiled language for readable, checked programs and
safe evolution of deployed systems. Developers use ordinary source files;
the long-term tools reason about types, contracts, dependencies, and semantic
changes.

The active implementation is a small Rust compiler using LLVM 19 through
Inkwell. The previous compiler, interpreter, runtime, and their dedicated
tooling have been removed from the active tree. Git history retains that work;
there is no second implementation to maintain.

## Start here

- [Build and run the compiler](compiler/README.md)
- [Project goals](docs/project/charter.md)
- [Language decisions](docs/rfcs/language-foundation.md)
- [Changes and deployment decisions](docs/rfcs/change-and-deployment.md)
- [Roadmap and self-hosting gates](ROADMAP.md)
- [Current implementation status](docs/project/implementation-status.md)
- [Documentation index](docs/README.md)

Accepted goals are not implementation claims. The native seed is not yet a
complete language implementation or a self-hosted compiler.

## Repository

- `compiler/src`: syntax, type/proof checking, and native code generation.
- `compiler/std`: standard-library source, compiled like application code.
- `compiler/runtime`: managed-memory and private platform primitives in Rust.
- `compiler/examples` and `compiler/tests`: runnable examples and focused tests.
- `docs`: goals, decisions, and current status.

All changes use pull requests. See [Contributing](CONTRIBUTING.md),
[Governance](GOVERNANCE.md), [Support](SUPPORT.md), and
[Security](SECURITY.md). Participation follows the
[Code of Conduct](CODE_OF_CONDUCT.md). Loom is [MIT licensed](LICENSE).
