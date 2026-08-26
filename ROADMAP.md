# Loom Roadmap

Loom is an experimental language implementation. This roadmap records current
engineering priorities, not release promises or delivery dates. Completed work
belongs in the [changelog](CHANGELOG.md) and
[implementation status](docs/project/implementation-status.md) rather than in
an ever-growing checklist here.

The current baseline already connects ordinary source files to name and type
checking, checked MIR, LLVM objects, native executables, an explicit interpreter
oracle, package graphs, a moving collector, lexical cleanup, structured tasks,
the CLI, formatter, and LSP.

## Now

### Unify concrete value lowering

Replace isolated scalar, record, list, text, coroutine, and dynamic-interface
fast paths with one typed layout plan that drives storage, calls, clone, trace,
and drop behavior.

Acceptance requires:

- one canonical machine-instance identity for each specialized layout;
- checked and erased boundaries that fail closed when specialization is unsafe;
- unchanged value equality, contracts, checked arithmetic, GC relocation, and
  concept behavior;
- IR and runtime tests that demonstrate the common hot paths do not allocate or
  enter universal helpers unnecessarily.

### Close checked-entry and assumed-body boundaries

Contract and invariant checks should execute once at the checked entry, after
which eligible private code may use a proved concrete ABI. Unknown or failing
paths must preserve their original fault, blame, and source location.

### Strengthen reproducible evidence

Keep Linux and macOS native closure tests, the configured Windows native gate,
fuzzing, package integrity tests, and base-versus-candidate PR benchmarks
reproducible. Expand performance evidence with fixed-host trends, warm and
incremental builds, peak memory, and profiler data before making broader claims.

## Next

### Broaden typed layouts

Apply the common layout plan to nested and managed records, known generic
instances, generic containers, direct `Text` values, coroutine slots, and
dynamic concept payloads. Optimize from profiles rather than adding
source-shape special cases.

### Improve incremental compilation

Move persistent reuse below whole-graph checked MIR where stable identities and
validation make module- and instance-granular reuse trustworthy. Cache hits and
cold builds must produce identical diagnostics, reachability, and behavior.

### Make resource obligations independently verifiable

Carry the canonical `MustScope` obligation identity into versioned checked MIR
so artifact and cache validation can verify it without trusting an earlier
semantic-analysis process.

### Expand host parity

Bring the full LLVM backend, linker, runtime I/O, debugger integration, and
native closure tests to Windows before advertising Windows native support or
publishing a Windows binary archive.

### Refine developer experience

Improve diagnostics, source debugging, formatter stability, LSP behavior, and
package workflows while keeping the CLI and LSP on one analysis pipeline.

## Later, with evidence

The following directions require a concrete use case and an independently
testable design before implementation:

- a Cranelift fast-development backend;
- WebAssembly/WASI artifacts and a defined host ABI;
- exact-width integer types for binary formats, SIMD, or FFI;
- a narrow FFI and explicit pin boundary;
- a multithreaded executor that preserves structured cancellation and cleanup;
- generators, streams, or task groups built on the existing coroutine runtime.

## Not planned for the current language core

- Rust-style ownership, borrow, lifetime, or `Pin` syntax;
- implicit runtime discovery from `any` to `dyn C`;
- GC finalizers, destructors triggered by collection, or observable addresses;
- open-world implementation registries or a stable plugin witness ABI;
- live programming, AST editing, AOP-style weaving, or an operator/reconciliation
  runtime;
- operator overloading or a custom machine-code backend.

These exclusions keep the current work focused on an ordinary compiled
language with explicit static semantics and conventional source-control tools.
