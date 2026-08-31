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

### Make `std` an ordinary source library

Move every public library policy and algorithm out of compiler name tables and
into compiler-distributed Loom source. The compiler may retain only private,
format-neutral primitives that ordinary source cannot implement, such as task
suspension, timer and I/O task creation, GC allocation, and target-specific
system boundaries.

Acceptance requires:

- public `std` calls resolve to ordinary source definitions and participate in
  normal reachability, monomorphization, contracts, and diagnostics;
- compiler primitives are unavailable to application and dependency source;
- public function names are absent from compiler builtin dispatch tables;
- unused library definitions, helpers, and data are absent from native objects;
- each migrated API deletes its former builtin/catalog path in the same change,
  without aliases, compatibility readers, or parallel implementations.

### Remove the unreachable universal runtime ABI

Native compilation now has one typed LCIR emitter. Delete the runtime-only
`ValueSlot` heap, shadow-root, witness, legacy Task, value-operations, and Int
list surfaces that no generated object can call. Preserve only helpers used by
typed LCIR, moving shared Text/JSON utilities to representation-neutral modules
before removing their old containers. Advance the runtime identity once, with
no aliases or dormant compatibility exports.

### Strengthen reproducible evidence

Keep the macOS development gate, cross-platform release closure, explicit fuzz
campaigns, package integrity tests, and opt-in base-versus-candidate benchmarks
reproducible. Expand performance evidence with fixed-host trends, warm and
incremental builds, peak memory, and profiler data before making broader claims.

## Next

### Grow the source library

Build collections, text, encoding, time, process, file, network, logging, and
data-format modules in Loom after their narrow primitives are established.
Task composition should use a general source-level associated-function and
tuple/list abstraction; it must not become a permanent compiler catalog keyed
by the public `Task.*` spelling.

### Improve incremental compilation

Move persistent reuse below whole-graph checked MIR where stable identities and
validation make module- and instance-granular reuse trustworthy. Cache hits and
cold builds must produce identical diagnostics, reachability, and behavior.

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
