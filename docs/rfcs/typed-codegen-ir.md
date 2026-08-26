# Typed code generation IR

Status: **Accepted; implementation in progress**

## Decision

Loom will place a target-aware, typed SSA representation between checked MIR
and native target emitters. This representation is Loom Codegen IR (LCIR).
Checked MIR remains the portable semantic boundary shared with the interpreter;
LCIR owns callable instances, physical value representations, explicit control
flow, and the information needed for mechanical target emission.

LCIR is compiler-private. Its Rust API, textual dump, physical ABI, and object
symbols may change with the compiler build. It is not a source IR, a stable
artifact format, an ownership system, or a public FFI ABI.

The standalone scalar foundation is described in the current
[Code generation IR internals](../internals/codegen-ir.md). The production
pipeline described by this record is not complete yet.

## Motivation

The production LLVM backend currently lowers checked MIR through a universal
value implementation and several closed-world native specializations. Scalar,
record, integer-range, and private-list paths repeat representation, proof,
call-compatibility, and runtime-requirement decisions inside the target
emitter. Some functions may acquire universal, checked-native, and
assumption-specialized bodies.

That structure makes a correct fast path depend on exact MIR shapes and makes
each additional type or operation multiply the number of lowering choices.
LLVM should receive already selected types, control flow, and proofs instead of
being asked to recover them from a tagged value representation.

## Accepted pipeline

The native pipeline will become:

```text
checked MIR
  -> command roots and closed-world reachability
  -> callable-instance plan
  -> target-aware representation plan
  -> LCIR construction
  -> independent LCIR validation
  -> checked LCIR
  -> target emitter
  -> target verification and optimization
  -> relocatable object
```

Each stage has one responsibility:

| Stage | Responsibility |
| --- | --- |
| checked MIR | Valid source-level types and executable semantics. |
| instance plan | The complete callable graph required by the selected roots. |
| representation plan | One physical representation for every live value and callable boundary. |
| LCIR construction | Explicit typed SSA instructions, blocks, parameters, edges, and origins. |
| LCIR validation | Structural, type, dominance, effect, control-flow, and representation invariants. |
| target emitter | Mechanical translation of checked LCIR to target IR. |

Target emission must not resolve names, reinterpret contracts, infer a physical
representation from an expression shape, or create new callable instances.

## Representation policy

Statically known values use direct representations by default. The initial
scalar vocabulary is:

- `Uninhabited` as control-flow vocabulary for `Never`, never as an SSA value;
- `Zst` for `Unit`;
- `Scalar(I1)` for `Bool`;
- `Scalar(I64)` for `Int`;
- `Scalar(F64)` for `Float`.

Aggregate, managed, dynamic-witness, erased, and coroutine representations are
added only with complete lowering and validation rules. A generic or dynamic
operation elsewhere in an artifact does not make an unrelated scalar carry a
universal tag.

Every representation change is explicit in LCIR. Genuine erased boundaries,
including `dyn C`, use purpose-specific representations. They do not preserve a
universal value envelope for ordinary typed code.

## Whole-artifact migration

Migration selection is atomic for one complete reachable artifact:

1. Compute roots, reachability, target layout, instances, representations, and
   support without mutating an LLVM module.
2. If every reachable item is supported, construct and validate the complete
   LCIR artifact, then emit only the LCIR route.
3. If valid checked MIR contains a reachable feature outside current LCIR
   coverage, emit the complete artifact through the legacy route.
4. Never connect an LCIR function to a legacy function through a universal
   wrapper.

An unsupported unreachable function does not affect route selection. For a
test artifact, the union reachable from every selected test is one artifact;
one unsupported reachable test dependency selects legacy emission for all of
it.

The implementation must distinguish unsupported coverage from defects:

- `Complete` means every reachable callable, witness edge, builtin, root
  harness, representation, and operation is supported.
- `Unsupported` means valid input lies outside migration coverage and permits
  whole-artifact fallback.
- Missing checked-MIR references, inconsistent plans, invalid generated LCIR,
  target-emitter failures, verifier failures, and object-write failures are
  compiler defects and must not fall back.

Route selection and every input that can change it are part of native-object
identity. The backend build fingerprint includes the LCIR implementation.

## Edge-result control flow

A fallible or suspending operation is a terminator, not an ordinary instruction
that returns a value beside an ignorable status. Its result exists only on the
normal or resume edge and is injected into the destination block's first
parameter. Forwarded edge arguments are modeled separately from that result.

The scalar fault slice adds forms equivalent to:

```text
checked_int operation, operands
    normal target(result; forwarded...)
    fault target(forwarded...)

invoke callee(arguments...)
    normal target(result; forwarded...)
    unwind target(forwarded...)

resume_fault
```

Checked addition, subtraction, and multiplication map to overflow-aware target
operations. Integer division and remainder first branch on division by zero and
the signed `MIN / -1` case, then execute a proved operation. The two source
faults remain distinguishable.

Validation prevents a checked result from being used on its fault edge and
prevents an unwind edge from returning normally. The target emitter does not
recognize an adjacent instruction-and-branch pattern to recover this meaning.
The same rule will apply to suspension and resume data.

## Scalar LLVM ABI

The first LLVM emitter uses one compiler-private ABI family for every LCIR
source function:

| LCIR representation | LLVM representation |
| --- | --- |
| `Zst` | empty struct |
| `Scalar(I1)` | `i1` |
| `Scalar(I64)` | `i64` |
| `Scalar(F64)` | `double` |

An infallible function returns its typed result directly. A function with the
`MAY_FAULT` effect returns `{ i32 status, T value }` and receives one trailing
opaque runtime-context pointer. Status zero is success; status one is a source
runtime fault. A normal return supplies `{ 0, value }`. A fault origin reports
the fault once and returns `{ 1, zero }`; an unwind continuation propagates the
failure without reporting it again.

All functions are declared before bodies are emitted, so direct and mutually
recursive calls use the same typed ABI. Entry block parameters map to function
parameters; non-entry block parameters map to phi nodes. The run or test
harness calls the typed root directly. It creates no runtime for an infallible
scalar root and creates a runtime, but no executor, for a synchronous faulting
root.

Calls to the C process entry, libc, and versioned Loom runtime functions are
explicit external boundaries. They do not permit two source-function ABIs in
one artifact.

## Target-emitter rules

The emitter consumes only checked LCIR and validated roots. It does not receive
MIR, universal value slots, argument-node lists, native-range plans, or legacy
runtime-requirement graphs.

It creates all functions and blocks first, translates values in SSA, records
edge inputs, and completes phi nodes after terminators are emitted. If two
edges from one source block target the same destination, normalization or an
edge block gives each phi input a unique LLVM predecessor. Ordinary scalar
function bodies do not allocate universal stack slots.

Proved integer add, subtract, and multiply may use LLVM's `nsw` operations.
Checked operations use signed overflow intrinsics. Float operations do not use
fast-math flags. Branch likelihood is explicit LCIR metadata or part of a
specific checked/invoke terminator; the emitter does not infer it from a body
shape.

Target-machine creation, module triple and data layout, debug-source mapping,
LLVM verification, optimization, object writing, runtime-bundle linking, and
host linking remain shared infrastructure.

## Migration and deletion gates

Legacy implementation is removed by demonstrated semantic coverage:

| Legacy area | Gate before deletion |
| --- | --- |
| Scalar native wrappers and universal scalar locals | LCIR covers reachable scalar signatures, locals, CFG, direct and fallible calls, checked faults, scalar contracts and cleanup exits, and run/test harnesses with zero eligible fallback. |
| Assumed integer bodies | General LCIR proofs preserve checked behavior inside and outside proved domains, recursive benchmarks remain competitive, and no emitted body depends on one recursion pattern. |
| Duplicated universal/native/assumed requirement scans | Runtime, collection, executor, and fault requirements are derived from checked LCIR and its closed instance graph. |
| Aggregate and private-storage specializations | General representation, escape, range, and scalar-replacement planning covers the same copy, mutation, bounds, GC, cleanup, and suspension behavior. |
| Universal function ABI and `ValueSlot` | LCIR covers aggregates, enums, refined values, generics, witnesses, `dyn`, contracts, builtins, moving GC, cleanup, async functions, Tasks, and all maintained native fixtures without fallback. |

New exact-shape native specializations are not accepted migration work. Range,
escape, liveness, devirtualization, and scalar-replacement facts belong in
planning or validated LCIR transforms.

## Acceptance criteria

Every LCIR slice requires:

- builder and independent validator tests, including malformed programs;
- stable dumps for the same checked program and insertion order;
- source-to-MIR-to-LCIR tests for every newly supported operation;
- interpreter/native differential tests for results and faults;
- LLVM structure tests for direct signatures, phi nodes, calls, checked
  operations, and fault propagation;
- negative IR assertions that a complete scalar artifact has no universal
  value, linked argument nodes, tag operations, GC setup, executor setup, or
  universal root wrapper;
- reachable-unsupported and unreachable-unsupported route tests;
- tests proving that lowering, validation, and target defects do not fall back;
- development and release verification on every supported native CI host;
- object-identity tests for LCIR format, planning, target-layout, root,
  reachability, or route-selection changes;
- controlled scalar benchmarks against the legacy compiler and the maintained
  Go, Rust, C, and C++ cases.

The migration is complete only after the legacy universal source-function ABI
and its exact-shape specializations are deleted. LLVM optimization output alone
is not evidence that typed lowering is complete.

## Non-goals

This record does not add source syntax, ownership or borrowing, AST editing,
live programming, AOP, runtime conformance search, operator dispatch, a stable
native library ABI, or a public serialization format for LCIR.
