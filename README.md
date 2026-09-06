# Loom

Loom is an experimental compiled language for readable, checked programs and
safe evolution of deployed systems. Developers use ordinary source files;
the long-term tools reason about types, contracts, dependencies, and semantic
changes.

The active frontend is written in Loom: it loads packages, parses source,
checks types and required proofs, and produces a checked native program.
A narrow Rust tool lowers that program through LLVM 19/Inkwell. A Rust seed
currently builds the first compiler stage; subsequent stages compile the same
Loom sources. Retiring the replaced seed frontend is next, not maintaining two
language implementations. The previous compiler and interpreter remain only
in Git history.

## Start here

- [Build and run the compiler](compiler/README.md)
- [Project goals](docs/project/charter.md)
- [Language decisions](docs/rfcs/language-foundation.md)
- [Changes and deployment decisions](docs/rfcs/change-and-deployment.md)
- [Roadmap and self-hosting gates](ROADMAP.md)
- [Current implementation status](docs/project/implementation-status.md)
- [Documentation index](docs/README.md)

Accepted goals are not implementation claims. Staged bootstrap does not mean
the complete language, standard library, or tooling design is implemented.

## Repository

- `compiler/src`: Rust bootstrap seed and the retained LLVM/platform bridge.
- `compiler/loom`: Loom-written compiler and reusable frontend packages.
- `compiler/std`: standard-library source, compiled like application code.
- `compiler/runtime`: managed-memory and private platform primitives in Rust.
- `compiler/examples` and `compiler/tests`: runnable examples and focused tests.
- `docs`: goals, decisions, and current status.

All changes use pull requests. See [Contributing](CONTRIBUTING.md),
[Governance](GOVERNANCE.md), [Support](SUPPORT.md), and
[Security](SECURITY.md). Participation follows the
[Code of Conduct](CODE_OF_CONDUCT.md). Loom is [MIT licensed](LICENSE).
