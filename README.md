# Loom

Loom is an experimental compiled language for readable, checked programs and
safe evolution of deployed systems. Developers use ordinary source files;
the long-term tools reason about types, contracts, dependencies, and semantic
changes.

The active frontend is written in Loom: it loads packages, parses source,
checks types and required proofs, and produces a checked native program.
A narrow Rust tool lowers that program through LLVM 22/Inkwell. An existing
Loom compiler bootstraps the current source; a pinned historical seed can be
built on demand when needed. The active tree has one language frontend, not a
permanent Rust basic version. Previous frontends and interpreters remain only
in Git history.

## Start here

Already built this checkout? Open the programming trial directly:

```sh
npm --prefix editors/vscode ci
npm --prefix editors/vscode run try
```

This opens the local VS Code development extension with a small file tool,
same-directory tests, and format/check/build/test/run tasks. See the
[trial setup](editors/vscode/README.md#try-it) for prerequisites and CLI-only use.

- [Try editing a small Loom program](editors/vscode/README.md#try-it)
- [Write and test a multi-package file tool](compiler/examples/wordcount/README.md)
- [Try typed tasks and postfix await](compiler/examples/tasks/README.md)
- [Suspend tasks with real timers](compiler/examples/timers/README.md)
- [Keep scoped cleanup across await](compiler/examples/async_cleanup/README.md)
- [Copy files with source Task APIs](compiler/examples/async_files/README.md)
- [Build and run the compiler](compiler/README.md)
- [Use Loom syntax and analysis libraries](compiler/loom/README.md#public-syntax-libraries)
- [Format source and get editor feedback](compiler/loom/README.md#formatting-and-editor-feedback)
- [Try the VS Code development extension](editors/vscode/README.md)
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
- `editors/vscode`: highlighting, compiler-backed diagnostics, and formatting.
- `docs`: goals, decisions, and current status.

All changes use pull requests. See [Contributing](CONTRIBUTING.md),
[Governance](GOVERNANCE.md), [Support](SUPPORT.md), and
[Security](SECURITY.md). Participation follows the
[Code of Conduct](CODE_OF_CONDUCT.md). Loom is [MIT licensed](LICENSE).
