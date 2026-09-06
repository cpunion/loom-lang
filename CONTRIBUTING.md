# Contributing to Loom

Use focused pull requests for every change, including maintainer work. Preserve
unrelated local changes. Do not add compatibility paths for unpublished
implementations or lower the accepted goals to match a temporary subset.

## Development

Use Rust 1.88, LLVM 19, and Clang. See the
[compiler guide](compiler/README.md) for setup and executable examples.
The root Cargo workspace contains the active compiler only.

Run the relevant local gate:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --workspace
cargo test --locked --workspace
node .github/scripts/check-docs.mjs
node --test .github/scripts/check-docs.test.mjs
```

Start with the narrowest test that exercises a change. A language change needs
source-to-native evidence and the important rejection case, not another public
interpreter or a large parallel test framework. Do not weaken contracts,
integer checks, resource cleanup, or proof obligations to pass a test.

macOS is the development gate. Other hosts require actual evidence before
support or release claims. Performance claims compare reproducible workloads
and inspect generated code, rather than adding indirection by default.

## Code and documentation

Keep one checked semantic model and add layers only for demonstrated consumers.
Put public algorithms and policy in Loom source; keep compiler/runtime
primitives narrow. Unsafe runtime boundaries must explain the relevant pointer
lifetime, layout, and collection guarantees.

Write documentation in English. Current behavior belongs in the compiler guide
or implementation status; accepted target decisions belong in the
[design records](docs/rfcs/README.md). Label incomplete syntax examples. Keep
links valid and update the [changelog](CHANGELOG.md) when behavior changes.

Commits should have one purpose and a conventional subject, such as
`feat(compiler): check generic records`. PRs state what changed, what remains
unsupported, and which exact checks were run. Avoid unrelated cleanup or
invented compatibility work. Never commit credentials, build outputs, or caches.

Report vulnerabilities privately under [Security](SECURITY.md).
Contributions are [MIT licensed](LICENSE) and follow the
[Code of Conduct](CODE_OF_CONDUCT.md).
