# Loom

Loom is an experimental compiled language for readable, checked programs and
safe evolution of deployed systems. Developers use ordinary source files;
the long-term tools reason about types, contracts, dependencies, and semantic
changes.

The active frontend is written in Loom: it loads packages, parses source,
checks types and required proofs, and produces a checked native program.
A narrow Rust tool lowers that program through LLVM 19/Inkwell. An existing
Loom compiler bootstraps the current source; a pinned historical seed can be
built on demand when needed. The active tree has one language frontend, not a
permanent Rust basic version. Previous frontends and interpreters remain only
in Git history.

## Start here

- [Build and run the compiler](compiler/README.md)
- [Use Loom syntax and analysis libraries](compiler/loom/README.md#public-syntax-libraries)
- [Project goals](docs/project/charter.md)
- [Language decisions](docs/rfcs/language-foundation.md)
- [Changes and deployment decisions](docs/rfcs/change-and-deployment.md)
- [Roadmap and self-hosting gates](ROADMAP.md)
- [Current implementation status](docs/project/implementation-status.md)
- [Documentation index](docs/README.md)

Accepted goals are not implementation claims. Staged bootstrap does not mean
the complete language, standard library, or tooling design is implemented.

## Repository

- `compiler/src`: Rust LLVM lowering, checked-artifact input, and host linking.
- `compiler/loom`: Loom-written compiler CLI and checked-artifact emission.
- `compiler/std`: source libraries, including the shared compiler analysis layers.
- `compiler/runtime`: managed-memory and private platform primitives in Rust.
- `compiler/examples` and `compiler/tests`: runnable examples and focused tests.
- `docs`: goals, decisions, and current status.

All changes use pull requests. See [Contributing](CONTRIBUTING.md),
[Governance](GOVERNANCE.md), [Support](SUPPORT.md), and
[Security](SECURITY.md). Participation follows the
[Code of Conduct](CODE_OF_CONDUCT.md). Loom is [MIT licensed](LICENSE).
