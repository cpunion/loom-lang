# LLVM backend

`loom-codegen-llvm` is the default native backend. Its object fingerprint,
object emission, and executable emission APIs require
`loom_mir::CheckedProgram`. It computes the closed-world source graph through
`loom-codegen-ir`, emits LLVM IR, verifies and optimizes it, and writes a
relocatable object. Linking is a separate driver operation.

## LLVM integration

The workspace currently targets LLVM 19 through:

- `inkwell 0.10.0` with `llvm19-1-prefer-dynamic`;
- `llvm-sys 191.1.0` with dynamic linking preferred.

Most compiler crates forbid unsafe Rust. `loom-codegen-llvm` denies it by
default and has one audited exception for Inkwell's typed GEP builder, whose
pointee/index proof comes from the private native-storage plan. Runtime FFI
implementation is isolated in `loom-runtime`, which explicitly permits the
unsafe operations required by the compiler-private C ABI.

Contributors need LLVM 19 development files and a matching `llvm-config`. The
workspace does not silently fall back to another LLVM major version.

## LCIR foundation status

The workspace contains a scalar typed-SSA foundation in `loom-codegen-ir`. The
LLVM backend depends on that crate for `SourceRoots` and
`ReachableSourceGraph`, but it does not consume the crate's LCIR
`loom_codegen_ir::CheckedProgram`. This does not weaken the production MIR
boundary: source graph and native entry points require
`loom_mir::CheckedProgram`. There is no MIR-to-LCIR lowering, and no production
artifact is selected, lowered, emitted, or cached through typed LCIR.

The implemented crate boundary is documented in
[Code generation IR](codegen-ir.md). The accepted pipeline design,
whole-artifact migration rule, typed ABI, and deletion gates are in the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md).

## Target-machine policy

For an implicit host target, the backend uses LLVM's normalized host triple and
the actual host CPU name/features. For any explicit `--target-triple`,
including one equal to the host triple, it uses `generic` CPU, an empty feature
set, PIC relocation, and the target's LLVM data layout.

The current universal native representation requires 64-bit pointers. A
32-bit data layout fails before object emission. An LLVM target being available
only establishes that an object can be emitted; it does not establish runtime,
linker, CI, or release support.

## Verification and optimization

The module triple and data layout are set before lowering. The pipeline is:

1. emit reachable functions, live witnesses, runtime declarations, and debug
   metadata;
2. run the LLVM verifier;
3. run the selected pass pipeline;
4. run the verifier again;
5. emit a relocatable object.

The current pass strings are:

| Profile | Pipeline |
| --- | --- |
| development | `default<O0>,globaldce` |
| release | `default<O2>,globaldce` |

Verifier or pass-manager failure is a compiler defect. Optimization must not
change checked overflow, value copy, contract, cleanup, GC, concept, or Task
semantics.

## Runtime requirements

After reachability, a fixed-point analysis classifies each reachable callable's
need to:

- raise a compiler/runtime fault;
- enter a moving-GC collection boundary;
- use the async executor.

These flags are compiler-private lowering facts, not source effects. They
allow a proven pure scalar native body to omit status and hidden runtime
context, a synchronous managed root to create only a runtime, and an async root
to attach an executor only when required.

## Native specialization

The universal value path remains the complete semantic implementation. Current
closed-world fast paths include primitive scalar calls, eligible flat
primitive-field records, narrowly proven checked integer recursion, and
non-escaping local `List[Int]` shapes.

Each optimization is fail-closed. Contracts, invariants, generic or managed
shapes, escapes, suspension, unsupported expressions, or an incomplete proof
fall back to universal lowering. These optimizations are not language ABI
promises and should not be copied into user reference material.

An exact single-append range over private `List[Int]` storage keeps its length
in SSA when the appended expression cannot reference the receiver. Generated
code publishes that length before allocation growth and on normal loop exit,
instead of writing the header on every iteration. Receiver-observing element
expressions retain eager commits. A fault may clean up a header whose length is
a safe lower bound: this is valid only for private contiguous `i64` elements,
which have no destructor or source-visible partially built value.

Optimization work requires both semantic differential tests and IR structure
tests. A benchmark improvement alone is insufficient evidence that a fast path
is correct.

## Object identity and linking

The native object fingerprint is format `loom-native-object-v4` and includes:

- compiler/backend build fingerprint and linked LLVM version;
- MIR format/version;
- exact target-machine identity and optimization;
- roots and reachability;
- complete type, concept, requirement, and prelude metadata;
- reachable function and live witness-slot data;
- stable debug source metadata.

The compiler build fingerprint includes the `loom-codegen-ir` crate sources.
There is no LCIR-specific object format or route-selection cache key yet.

Host linking uses the Rust runtime archive embedded in the compiler build.
Cross-target linking accepts only a validated matching runtime bundle and an
explicit linker. Final native executables are not persistently cached because
the link environment is not yet hermetic.

## Debug information

The backend emits source line information from stable project-relative paths.
Linux executables retain DWARF in the ELF output. On macOS, `dsymutil --verify`
produces a sibling `.dSYM` bundle. `loomc debug` keeps temporary executable and
debug data alive for the debugger session and launches in the project root.

There is no stable native library, debugger pretty-printer, plugin, or FFI ABI
in the current implementation.
