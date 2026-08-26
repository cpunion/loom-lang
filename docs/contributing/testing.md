# Testing

Tests should establish the narrowest invariant at the cheapest layer and then
add end-to-end evidence for observable behavior. A large fixture does not
replace a focused regression test.

## Full workspace gate

With LLVM 19 configured:

```sh
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 check --locked --workspace --all-targets
cargo +1.88.0 clippy --locked --workspace --all-targets -- -D warnings
CARGO_ENCODED_RUSTFLAGS='-Ctarget-cpu=generic' \
  cargo +1.88.0 build --locked -p loom-runtime
cargo +1.88.0 build --locked -p loom-cli
runtime_bundle_root="$(mktemp -d)/runtime"
target/debug/loomc runtime pack \
  --archive target/debug/libloom_runtime.a \
  --output "$runtime_bundle_root"
export LOOM_RUNTIME_BUNDLE="$runtime_bundle_root"
cargo +1.88.0 test --locked --workspace --all-targets
cargo +1.88.0 build --locked --workspace --all-targets
```

Use `--locked` so dependency resolution matches CI.
The runtime preparation is needed only by native link/run tests. A fresh
`cargo check` or `cargo build -p loom-codegen-llvm` has no runtime sidecar and
must not create a nested `runtime-target`.

## Focused commands

Examples:

```sh
cargo +1.88.0 test --locked -p loom-syntax
cargo +1.88.0 test --locked -p loom-sema
cargo +1.88.0 test --locked -p loom-mir --test checked_mir
cargo +1.88.0 test --locked -p loom-runtime
cargo +1.88.0 test --locked -p loom-codegen-llvm --test native
cargo +1.88.0 test --locked -p loom-codegen-llvm --test runtime_bundle
cargo +1.88.0 test --locked -p loom-cli --test cli
```

Run a single Rust test by adding its name and `--exact` when appropriate.

## End-to-end source loop

For a language fixture, exercise both backends rather than only parsing it:

```sh
for backend in llvm interpreter; do
  cargo +1.88.0 run --locked -p loom-cli -- \
    --backend "$backend" --no-cache check examples/core03
  cargo +1.88.0 run --locked -p loom-cli -- \
    --backend "$backend" --no-cache test examples/core03
  cargo +1.88.0 run --locked -p loom-cli -- \
    --backend "$backend" --no-cache run examples/core03
done
```

The LLVM and interpreter artifacts are different. Build and execute each using
the matching backend:

```sh
cargo +1.88.0 run --locked -p loom-cli -- \
  --no-cache build --output target/example examples/core01
cargo +1.88.0 run --locked -p loom-cli -- \
  run --artifact target/example

cargo +1.88.0 run --locked -p loom-cli -- \
  --backend interpreter --no-cache build \
  --output target/example.loomi examples/core01
cargo +1.88.0 run --locked -p loom-cli -- \
  --backend interpreter run --artifact target/example.loomi
```

## What to test by layer

### Syntax and diagnostics

Test accepted syntax, recovery after the error, exact source spans, stable
diagnostic codes, and formatter idempotence. Keep invalid examples minimal.

### Semantics and lowering

Test both the positive proof and a nearby counterexample. For a removed runtime
check, include:

- a statically proved case;
- a source rejection when the boundary is statically invalid;
- a dynamic failure when proof is unavailable;
- a case that must retain the checked fallback.

### MIR

Every new MIR shape needs valid round-trip coverage and directly constructed
malformed programs for the validator. Do not rely on the parser being unable to
produce bad MIR; artifacts and caches are untrusted inputs.

### Interpreter and LLVM

Use differential tests for observable output, returned values, contracts,
faults, cleanup order, dynamic dispatch, and Task results. LLVM fast paths also
need IR assertions that prove the intended machine structure is present and
the checked fallback remains where required.

### Runtime

Force GC and suspension at inconvenient boundaries. Cover malformed
descriptors, stale registrations, cancellation, cleanup after fault, first
fault preservation, resource transfer, and runtime/executor lifecycle.

### Projects, registries, and caches

Use temporary directories and local loopback servers. Cover path traversal,
symlinks, oversized input, identity/checksum mismatch, credential redaction,
changed versions already present in the validated cache, corrupted cache
records and blobs, offline misses, and atomic materialization.

## Controlled quality gate

```sh
cargo +1.88.0 run --locked --release -p loom-quality \
  > target/c3-evidence.json
```

This is an end-to-end regression gate with generous time budgets. It does not
replace focused tests and is not a general benchmark.

## CI parity

Linux runs the full workspace and all principal fixture gates. macOS runs the
full workspace, standard-library differential tests, and the C3 dual-backend
loop. A complete Windows native job is configured, but it is not verified
platform or release evidence until a real Windows runner result is recorded.
See [Implementation status](../project/implementation-status.md) before adding
a platform claim.
