# Loom documentation

Loom is an experimental language and toolchain. These pages describe behavior
that is implemented and exercised by the repository's tests. They are not a
stability or compatibility promise: language syntax, artifacts, diagnostics,
and runtime interfaces may change while the project is under active research.

## Start here

- [Installation](getting-started/installation.md) explains the supported host
  configurations and how to build `loom` and `loom-lsp` from source.
- [Quick start](getting-started/quick-start.md) checks, builds, tests, and runs
  the executable Core fixtures with both backends.
- [Project layout](getting-started/project-layout.md) introduces `loom.toml`,
  directory packages, targets, dependencies, and generated files.

## Language guides

- [Language tour](guide/language-tour.md)
- [Constraints and contracts](guide/constraints-and-contracts.md)
- [Concepts and polymorphism](guide/concepts-and-polymorphism.md)
- [Resources and cleanup](guide/resources-and-cleanup.md)
- [Asynchronous programming](guide/asynchronous-programming.md)
- [Packages and dependencies](guide/packages-and-dependencies.md)

The guides distinguish source-level guarantees from implementation details.
When physical layouts or runtime interfaces are mentioned, assume they are
compiler-private unless the page explicitly says otherwise.

## Reference

- The [language reference](reference/language/README.md) defines observable
  version 0.4 syntax, typing, contracts, resources, tasks, and failures.
- The [standard library reference](reference/std/README.md)
  catalogs implemented values and imported operations.
- The [toolchain reference](reference/toolchain/README.md) defines CLI,
  manifest, registry, artifact, cross-compilation, and cache behavior.

Reference pages are normative for the version or format they identify. A
compiler-private layout described elsewhere is not a source or FFI guarantee.

## Compiler and runtime internals

The [compiler internals index](internals/README.md) covers the pipeline, MIR and
validation, reachability, LLVM, value layouts, GC, async runtime, and incremental
cache. These pages explain the current implementation for contributors; they do
not define a second language specification.

## Contributor documentation

- [Development setup](contributing/development.md)
- [Testing](contributing/testing.md)
- [Benchmarking](contributing/benchmarking.md)
- [Fuzzing](contributing/fuzzing.md)
- [Documentation](contributing/documentation.md)
- [Releases](contributing/releases.md)

Read the root [contribution guide](../CONTRIBUTING.md) before opening a pull
request.

## Project information

- [Project charter](project/charter.md)
- [Implementation status](project/implementation-status.md)
- [Quality policy](project/quality-policy.md)
- [Terminology](project/terminology.md)
- [Versioning and compatibility](project/versioning.md)
- [Roadmap](../ROADMAP.md) separates working behavior from planned work.
- [Changelog](../CHANGELOG.md) records user-visible changes from the current
  development cycle onward.
- [Governance](../GOVERNANCE.md) describes project roles and decision making.
- [Support](../SUPPORT.md) routes usage questions and bug reports.
- [Security policy](../SECURITY.md) explains how to report a vulnerability
  privately.
- [Code of Conduct](../CODE_OF_CONDUCT.md) sets expectations for participation.
- [Design records](rfcs/README.md) contain active proposals without presenting
  them as implemented features.

## Reading experimental documentation

The compiler and tests are the final evidence for the current implementation.
Examples under `examples/constraints-contracts`, `examples/concepts-polymorphism`,
`examples/async-resources`, and `examples/packages` are kept in the repository's
check/build/test/run closure and are the best executable companions to these
pages.

The [roadmap](../ROADMAP.md) contains proposals and incomplete work. A roadmap
item must not be read as an available feature. If a guide and the current
compiler disagree, please open a documentation issue or pull request with a
minimal reproducer.
