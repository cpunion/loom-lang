# LLVM backend

`loom-codegen-llvm` is the default native backend. Its production API prepares
one opaque object plan from `loom_mir::CheckedProgram`, owned emission options,
and an atomic route policy. The plan owns the exact LLVM target machine and
either a complete checked LCIR artifact or the checked-MIR roots and reachable
graph for one legacy object. Fingerprinting and emission consume that same
plan. Linking is a separate driver operation.

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

The workspace contains a direct typed-SSA foundation in `loom-codegen-ir` for
primitive values, literal-proven immortal `Text`, structural tuples, closed
records, and established transparent refined values.
Tuples and records are recursive acyclic products of other direct values and
may contain one another.
The LCIR emitter accepts only a closed `CheckedArtifact`: its roots, callable
closure, representations, CFG, types, proof-boundary shapes, and exact fault
effects have already crossed independent validation. Predicate truth itself is
a process-local conclusion supplied by fresh checked MIR; LCIR does not
re-prove an omitted source predicate. A decoded `Recheck` is outside LCIR
coverage and selects the complete legacy route. The emitter declares every
source function with its typed LCIR ABI, keeps source symbols internal, emits a
run or ordered test harness, verifies before and after optimization, and writes
a relocatable object.

Ordinary `build`, `run`, and `test` use `NativeRoutePolicy::Automatic`. Route
preparation creates one target machine and attempts the complete direct
lowering exactly once. `Complete` retains only the checked artifact and selects
LCIR. Only `Unsupported` constructs and stores `SourceRoots` plus
`ReachableSourceGraph` for a complete legacy object. Unsupported unreachable
code cannot change the route; one unsupported reachable test changes the
whole ordered-test artifact. Invalid roots, resource limits, compiler defects,
and LCIR emitter failures never fall back.

Source contracts are outside that routing slice. Hand-built LCIR can carry the
generic `ContractFailed` fault code, but LCIR does not yet preserve the contract
category, user code, contract span, and blame span required by production
diagnostics. The direct lowerer reports contracts as `Unsupported` until that
metadata has a checked LCIR representation and differential tests.

The implemented crate boundary is documented in
[Code generation IR](codegen-ir.md). The accepted pipeline design,
whole-artifact migration rule, typed ABI, and deletion gates are in the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md).

## Prepared object boundary

The production facade consists of:

- `prepare_native_object`, which owns `EmitOptions`, creates the target, and
  selects one immutable route;
- `prepared_native_object_fingerprint`, which hashes the stored route without
  repeating lowering or reachability;
- `prepared_native_target_identity`, which exposes the exact read-only target
  identity to runtime-bundle validation;
- `emit_prepared_native_object`, which borrows the same target machine and
  selected representation.

`PreparedNativeObject` is opaque and remains on the thread that prepared it;
the contained Inkwell target machine is not made artificially sendable. The
legacy and LCIR emitters retain low-level direct APIs for focused tests and
library clients, but each is a thin create-target-then-emit wrapper. Production
CLI paths use only the prepared facade.

Preparation failures have four structured classes: invalid root, resource,
target/configuration, and compiler defect. The CLI maps them respectively to
failure, failure, usage, and defect exits. Classification never depends on
matching diagnostic strings.

## Target-machine policy

For an implicit host target, the backend normalizes the standard target triple
that the compiler itself was built for and uses the actual host CPU
name/features. It does not use LLVM's OS-version-qualified runtime default;
otherwise a macOS bundle could become tied to the packer's Darwin point
version. For any explicit `--target-triple`, including one equal to the host
triple, it uses `generic` CPU, an empty feature set, PIC relocation, and the
target's LLVM data layout.

The target machine is created before representation selection. Its pointer
width is converted with checked arithmetic into `TargetLayout`. A complete
direct LCIR object whose representations are all width-independent can
therefore be emitted for a matching 32-bit LLVM target. `ImmortalText` and the
legacy universal representation both require 64-bit pointers; reachable text
selects atomic legacy fallback during 32-bit direct classification, and that
legacy route then reports its existing unsupported-target boundary. None of
these cases establishes 32-bit runtime, linker, CI, or release support; LLVM
target availability proves only object emission.

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
allow a proven pure direct native body to omit status and hidden runtime
context, a synchronous managed root to create only a runtime, and an async root
to attach an executor only when required.

## Direct LCIR products

LCIR `Product` values become literal LLVM structs whose fields recursively use
their validated direct types. Tuple and record construction and functional
record mutation use `insertvalue`; projection and tuple destructuring use
`extractvalue`. Parameters and ordinary results pass these structs by value,
and block parameters become aggregate phi nodes. LCIR source functions do not
allocate a universal value, tuple node, private record box, or GC object for
this representation.

A mutable inherent receiver is represented as one functional inout value. An
infallible call with source result `T` and ordered writebacks `W...` returns
`{ T, W... }`. A fallible call returns `{ i32 status, T, W... }` and receives
the usual fault-context pointer. Both normal and fault exits carry the current
receiver value, so a mutation completed before a later fault remains visible
to the caller. An admitted projected receiver is passed as the same direct leaf
product. Its returned leaf is inserted through the statically typed field path
on the normal edge and before fault propagation on the unwind edge. LLVM sees
only `extractvalue`, `insertvalue`, direct aggregate values, and the existing
functional return ABI; no proxy allocation, universal value, or runtime
writeback helper is introduced. Protected/managed projections, contracts, and
runtime-checked construction still select atomic whole-artifact fallback.

Fresh-source proven record invariants and refined predicates do not add an LLVM
wrapper or check. LCIR retains their distinct semantic types and proof opcodes,
while the emitter forwards the already established physical SSA value. A
refined scalar therefore uses the base scalar ABI; a refined product uses the
base product ABI; and an invariant record uses its field product ABI. Unknown
construction proofs still return language `Result` values on the legacy route.
Serialized proof rechecks retain their nominal result shape but also use the
legacy route, where failure is the canonical `ArtifactProofRejected` runtime
fault.

The current debug-info boundary describes that physical ABI as well. A
transparent scalar is reported as its base scalar debug type, and transparent
or invariant products use compiler-private physical product types; LLVM debug
metadata does not yet synthesize nominal source aliases such as `Money` or
`Range`. This deliberate display limitation does not erase nominal identity
from LCIR dumps, validation, cache fingerprints, or object artifact identity.

## Direct LCIR literal text

An admitted `Text` SSA value is one opaque LLVM pointer to an immutable object
emitted as a private, unnamed-address global. The global uses the runtime's
existing text-object prefix—layout descriptor pointer, allocation size, UTF-8
byte length, Unicode scalar length—followed by the exact UTF-8 bytes. It points
at the existing `loom_layout_text_v1` descriptor and lives for the process
lifetime. The pointer is not a universal `loom.Value`, a tagged interface
value, or an address whose identity is visible to source code.

LCIR loads scalar length directly from the immutable header. Containment calls
the existing allocation-free `loom_runtime_text_contains` byte-slice helper.
Equality and inequality compare content by combining byte length with that
helper; LLVM never compares literal object pointers to implement source
equality. The helper and descriptor declarations retain their exact target
pointer ABI when emitting 64-bit ELF, Mach-O, or COFF objects.

The complete artifact proof is intentionally narrow. Roots accept no source
arguments, every source callable has internal linkage, direct-call closure is
exactly validated, and `TextLiteral` is the only admitted producer. A text
pointer can therefore cross direct function, return, block-parameter, local,
or concrete generic boundaries only after originating in the same artifact's
immortal literal. These Text operations add no universal value,
GC/shadow-stack setup, runtime-object creation, or executor/scheduler setup;
independent fault effects elsewhere in the artifact may still require the
ordinary fault runtime context.

`concat`, `get`, dynamically created or otherwise moving text, and `Text`
nested in a product, sum, or transparent representation remain atomic
whole-artifact fallback. Supporting them requires a typed shadow-root ABI that
lets the moving collector update direct managed pointers at every safepoint;
this literal-only slice does not weaken that requirement.

## Direct LCIR closed sums

LLVM derives every sum layout from the checked `SumRepr` and target data. A
single variant is its payload struct with no tag. A multi-variant enum whose
variants have no payload fields is only the smallest checked integer tag. All
other sums are `{ tag, carrier }`. The carrier has the maximum payload ABI
size, the maximum payload ABI alignment, and the required tail padding. A
zero-length array of the most-aligned payload type imposes alignment without
adding storage; target-data checks reject any disagreement between the planned
and actual carrier size or alignment.

`SumConstruct` builds payload fields in source order. `SumSwitch` extracts the
tag once, switches exhaustively, and decodes the selected carrier into typed
payload block parameters. Temporary typed carrier storage is an LLVM lowering
detail: the release optimization gate requires SROA to remove every such
`alloca` and forbids `memcpy`, the universal `loom.Value`, runtime/GC/executor
symbols for pure sums, and indirect calls.

The test harness consumes the checked artifact's `TestOutcomePlan`. `Unit`
tests pass after a successful call. `Result[Unit, E]` tests compare the physical
tag with the explicit success variant; the explicit failure variant produces a
normal failed-test status. A source `RuntimeFault` is checked independently
before the result tag and retains the existing runtime-failure behavior.

## Legacy native specialization

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

Object identities are route-separated:

- `loom-lcir-native-object-v4` streams the canonical checked-artifact identity;
- `loom-legacy-native-object-v5` includes the run/test harness kind, MIR
  format, exact roots and source reachability, reachable functions, live
  witness slots, and the semantic type/concept/prelude tables used by legacy
  lowering.

Both include the compiler/backend build fingerprint, linked LLVM version,
native runtime ABI, exact normalized triple and data layout, CPU and feature
policy, implicit-versus-explicit target selection, optimization pipeline, PIC
relocation, and stable debug-source metadata. Output and LLVM-IR side-artifact
paths are excluded. A requested IR side artifact bypasses the object cache so
the file is always produced. The CLI object-cache domain is independently
versioned as `loom-llvm-object-cache-v9` and never suppresses fingerprint
errors.

The LCIR domains advance because literal text adds a new IR encoding and
one-pointer physical ABI. The emitted globals and containment calls reuse
runtime ABI symbols that were already versioned in the native runtime
identity, so the native runtime ABI itself does not advance for this slice.

Every executable link consumes one validated runtime bundle; the compiler
contains no runtime archive and its build script never starts Cargo. The CLI
discovers a host bundle from an explicit option, the environment, or the
installed sibling directory. Cross-target linking additionally requires an
explicit linker. Object emission is independent of this link input. Final
native executables are not persistently cached because the system linker, SDK,
and debug-companion environment are not yet hermetic.

## Debug information

The production checked-MIR backend emits source line information from stable
project-relative paths. Linux executables retain DWARF in the ELF output. On
macOS, `dsymutil --verify` produces a sibling `.dSYM` bundle. `loomc debug`
keeps temporary executable and debug data alive for the debugger session and
launches in the project root. LCIR publishes compile-unit, file,
`DISubprogram`, physical callable-signature, formal-parameter, parameter-value,
and instruction-location metadata. LCIR does not retain source parameter names,
so visible parameters have stable debugger names `arg0`, `arg1`, and so on.
Debug-source file IDs must be unique and must cover every emitted `Origin`;
missing or duplicate identities are compiler errors rather than mappings to the
primary file at an invented `(1, 1)` location. Hand-built LCIR using a synthetic
origin must therefore provide that generated file explicitly when requesting
debug information.

The signature deliberately describes the exact compiler ABI rather than a
logical wrapper that does not exist. Direct products use stable compiler-private
`LoomProduct<tN>` names because LCIR does not retain source record names; their
members, size, alignment, and offsets come from LLVM target data. Closed sums
similarly use `LoomSum<tN>` and describe their exact tagless, tag-only, or
tagged physical ABI. Tagged carrier and tag fields are artificial debug members
with target-data-derived sizes, alignments, and offsets. An infallible
inout callable returns `{ value, writebacks... }`, while a fallible callable
returns `{ status, value, writebacks... }` and receives an artificial trailing
`LoomFaultContext*` parameter. Status and writeback members are artificial.
These names describe compiler implementation types, not Loom source types or a
stable native ABI. In particular, a debugger's step-out result is the complete
physical aggregate; it must not interpret the status field as the logical
result. `loomc debug` uses the same atomic automatic route as build, run, and
test. Development optimization alone is not a debugger contract and does not
disable LCIR.

MSVC-targeted objects carry the LLVM `CodeView` module flag, and the linker is
given `/DEBUG` and an explicit staged `/PDB:` output. The configured Windows
gate checks typed-LCIR COFF and PDB structures, but source-level debugger
behavior remains partial and is not claimed until a native debugger test
exists.

There is no stable native library, debugger pretty-printer, plugin, or FFI ABI
in the current implementation.
