# Contributing to Loom

Thank you for helping improve Loom. The project welcomes compiler changes,
runtime work, tests, documentation, performance investigations, and carefully
scoped language-design proposals.

Loom is experimental. A small, test-backed change that preserves clear
semantics is more valuable than a broad feature with uncertain boundaries.

## Use a pull request

Every repository change, including a maintainer change, must go through a pull
request. Do not push feature, fix, documentation, or release changes directly
to `main`.

A pull request should:

- have one coherent purpose;
- explain the observable behavior before and after the change;
- include tests proportional to the risk;
- update user-facing documentation and the changelog when behavior changes;
- avoid unrelated formatting or refactoring;
- pass all required checks before merge.

Security reports are different. Follow [SECURITY.md](SECURITY.md) instead of
opening a public issue or pull request for an undisclosed vulnerability.

## Development prerequisites

The repository pins the following primary toolchain:

- Rust 1.88.0 with `rustfmt` and Clippy;
- LLVM 19 development libraries and `llvm-config`;
- Clang as the host linker for native Loom executables;
- Git.

See [the installation guide](docs/getting-started/installation.md) for Linux and
macOS setup. Windows currently supports development and testing of the
platform-independent compiler crates only; the complete LLVM/native runtime
path is not yet a supported Windows workflow.

Go, a C compiler, and a C++ compiler are optional unless you are changing the
cross-language benchmark runner.

## Build the workspace

From the repository root:

```sh
cargo +1.88.0 build --locked --workspace --all-targets
```

During development, prefer a targeted package or test first. Run the complete
gate before requesting review:

```sh
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 check --locked --workspace --all-targets
cargo +1.88.0 clippy --locked --workspace --all-targets -- -D warnings
cargo +1.88.0 test --locked --workspace --all-targets
cargo +1.88.0 build --locked --workspace --all-targets
```

Do not weaken a lint, verifier, contract, overflow check, cleanup rule, or test
oracle to make a change pass. If a gate is wrong, demonstrate that independently
and change it with an explicit regression test.

## Test the language closure

The three core fixtures exercise distinct language layers. Test both backends
when changing observable semantics:

```sh
for fixture in constraints-contracts concepts-polymorphism async-resources; do
    target/debug/loomc --no-cache check "examples/$fixture"
    target/debug/loomc --no-cache test "examples/$fixture"
    target/debug/loomc --no-cache run "examples/$fixture"
    target/debug/loomc --backend interpreter --no-cache test "examples/$fixture"
    target/debug/loomc --backend interpreter --no-cache run "examples/$fixture"
done
```

The LLVM verifier, checked-MIR validator, interpreter differential tests,
runtime tests, package tests, and LSP protocol tests are part of the correctness
boundary. Add the narrowest deterministic regression test that would have
caught the defect.

## Language and public-tooling changes

A change to accepted source, typing, contracts, diagnostics, package behavior,
standard-library behavior, CLI output, or artifact interpretation must include:

1. a concise statement of the rule and its non-goals;
2. positive and negative tests at the earliest reliable layer;
3. end-to-end evidence on every affected backend;
4. updates to the authoritative English documentation;
5. an entry under `Unreleased` in [CHANGELOG.md](CHANGELOG.md).

Do not add compatibility syntax, migration-only AST nodes, silent fallback, or
an interpreter escape path unless the project has explicitly accepted that
policy. Unsupported input should fail clearly and deterministically.

Large language proposals should begin as a focused design discussion with
examples, rejected alternatives, DCE/runtime consequences, and an executable
acceptance test. A proposal is not implemented functionality until parser,
checker, MIR, both relevant execution paths, diagnostics, and documentation
agree.

## Runtime and unsafe boundaries

The Rust workspace forbids unsafe code by default. The native runtime and ABI
have narrowly reviewed unsafe boundaries. Changes there should document:

- the ownership and lifetime of every raw pointer or handle;
- GC root and relocation behavior at allocation boundaries;
- cancellation and cleanup behavior;
- platform-specific assumptions;
- failure behavior for malformed compiler or runtime input.

Never place credentials in source, test fixtures, logs, diagnostics, benchmark
reports, or recorded artifacts. Registry tests must use synthetic tokens and
loopback services.

## Performance changes

Correctness and language semantics take precedence over benchmark results.

When changing a hot path:

- include an IR, allocation, or instruction-level explanation of the expected
  change;
- compare base and candidate with the repository benchmark runner on the same
  host and toolchain;
- preserve raw samples and report host-load warnings;
- do not present a shared-runner wall-clock result as a general language rank;
- add a structural regression test when the optimization has a checkable shape.

Pull requests run a base-versus-candidate benchmark workflow. Its comment is
diagnostic evidence, not an automatic acceptance threshold.

## Documentation

Repository documentation is written in English. Prefer short paragraphs,
descriptive headings, relative links, and examples that correspond to checked
fixtures. Keep these boundaries clear:

- guides explain how to use implemented behavior;
- reference material defines observable behavior;
- compiler internals describe replaceable implementation choices;
- the roadmap describes unimplemented work.

Do not copy implementation status into several documents. Link to one source of
truth. Run a local link check when moving or renaming pages.

## Commits and review

Use the repository's conventional commit style where practical, for example:

```text
feat(sema): reject ambiguous concept projections
fix(runtime): preserve the first task fault during cleanup
perf(codegen): pass eligible records by value
docs(guide): explain checked constrained construction
```

Keep commits reviewable and do not rewrite unrelated user work. Reviewers may
request a smaller change, stronger negative tests, clearer failure behavior, or
evidence that an optimization fails closed.

By participating, you agree to follow the
[Code of Conduct](CODE_OF_CONDUCT.md). Contributions are accepted under the
repository's [MIT License](LICENSE).
