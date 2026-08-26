# Code generation IR

`loom-codegen-ir` owns two code-generation boundaries. Its source-graph module
selects checked-MIR function roots and computes the closed-world source graph
used by production native compilation. Separately, its LCIR foundation
provides target-aware scalar representations, whole-artifact checked-MIR
lowering, typed SSA data structures, builders, independent program and
artifact-root validators, and a textual dump for tests and review.

`loom-codegen-llvm` consumes the resulting `CheckedArtifact` directly and emits
its scalar functions and run/test harness without the universal value ABI or
an executor. Its production prepared router attempts that whole-artifact
lowering once. `Complete` selects only typed LCIR; only `Unsupported` stores a
source reachability graph and selects the complete legacy emitter. Both routes
have independent object identities. The remaining LCIR coverage and deletion
gates are in the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md).

LCIR is compiler-private and target-specific. It is not a source IR, a public
artifact format, or a stable native ABI.

## Checked-MIR source graph

`SourceRoots` contains MIR `FunctionId` values selected for one command.
`analyze_source_reachability` closes direct calls, constructed witnesses,
dynamic requirement slots, and builtins into a deterministic
`ReachableSourceGraph`. These names deliberately include “source”: lowered
artifact roots use LCIR `InstanceId` values and are a different graph.
Root selection and graph analysis require `loom_mir::CheckedProgram`; this
module has no public raw-MIR compatibility entry point.

The graph records only ordered maps and sets and retains its existing Serde
field order because it participates in native-object fingerprints. Invalid
MIR references discovered while closing caller-supplied roots produce a
structured `GraphError`; the LLVM boundary maps that error into its backend
diagnostic without making source-graph analysis depend on LLVM. References
inside the program have already crossed the independent MIR validator.

## Current scalar representation catalog

`TargetLayout` currently records only pointer width. The canonical
`RepresentationPlan` contains:

| Loom type | LCIR representation |
| --- | --- |
| `Never` | `Uninhabited` |
| `Unit` | `Zst` |
| `Bool` | `Scalar(I1)` |
| `Int` | `Scalar(I64)` |
| `Float` | `Scalar(F64)` |

`Uninhabited` is catalog vocabulary only. The validator rejects it in function
signatures and SSA values. Aggregate, managed, list, dynamic-witness, and Task
representations are not implemented in this crate.

`TargetLayout::new` accepts nonzero, byte-sized pointer widths no greater than
128 bits. Acceptance by this standalone type is not a Loom native-target claim;
the production LLVM backend and runtime retain their own supported-target
boundary.

## Current program model

`ProgramBuilder` declares functions and produces an unchecked `Program`.
`Program::into_checked`, `ProgramBuilder::finish_checked`, and
`check_program` cross the independent validation boundary and return a
`CheckedProgram`.

`ArtifactRootRequest` selects either one run root or an ordered, possibly
empty test-root list. `check_artifact` independently checks branded function
identity, existence, duplicate tests, the zero-parameter `Unit` root signature,
and exact direct/invoke callable closure. It then returns a `CheckedArtifact`
which owns both the checked program and privately checked roots. The independent
LLVM object API consumes that wrapper without accepting unchecked roots or
falling back to checked MIR.

`artifact_identity` and `write_artifact_identity` expose a deterministic,
compiler-private identity for that complete checked artifact. Schema 2 carries
the `typed-lcir-whole-artifact` route tag, artifact kind, ordered run or test
roots, and the canonical LCIR dump with origins enabled. The payload therefore
includes the target, representation, and instance plans, checked functions and
control flow, operations, and complete function, instruction, and terminator
origins.
The dump uses explicit enum spellings and string escaping rather than Rust
`Debug`. Dense numeric IDs are content, but the process-local generative
`ProgramBrand` is deliberately excluded, so independently built artifacts with
the same deterministic numbering and content have the same identity. The
production LCIR fingerprint streams this identity together with backend,
target-machine, optimization, runtime ABI, and debug-source identities.

The callable-instance plan changed the compiler-private artifact identity but
not the emitted machine ABI. Advancing the artifact identity to schema 2 is
therefore the complete persistent-cache invalidation for this change because
that identity is an input to the native-object fingerprint. The
`loom-lcir-native-object-v1` format tag does not require an independent bump.

`lower_scalar_artifact` accepts a checked MIR program, a source run/test
request, and a target layout. It first selects `SourceRoots`, closes them with
`analyze_source_reachability`, and classifies every reachable function before
allocating LCIR. It returns either one complete independently checked
`CheckedArtifact` or one deterministic `SupportReport` for the whole artifact.
Invalid roots, resource limits, source-graph defects, and invalid generated
LCIR are structured `LoweringError` values and never select fallback.

The initial lowering coverage is monomorphic synchronous scalar signatures,
constants, scalar locals and assignment, blocks and conditionals,
short-circuit Boolean operations, integer ranges, pure scalar operations,
checked integer arithmetic, and direct/readonly-inherent scalar calls including
recursion. A dense reverse-call worklist computes the least fault-effect fixed
point in linear time and chooses direct calls versus fallible invokes. Cleanup
registration and assertions are conservatively unsupported together until
their complete normal/return/fault ladders can be emitted.

Lowering constructs canonical SSA directly: a single continuing branch does
not gain a join, values already dominating every predecessor do not gain
identity block parameters, short-circuit skip edges reuse the evaluated left
operand, and a range header carries only locals written or moved on a
continuing body path. These are generic control-flow/dataflow rules rather than
cleanup left for a later LLVM optimizer.

Per-function SSA environments are persistent sparse radix roots. Branches
share their entry root, local writes copy one bounded path, and joins compare
only subtries that differ from the shared entry. Range headers start from the
same environment and inspect only the body's continuing mutation set. This
keeps lowering proportional to emitted control flow and changed locals instead
of multiplying every branch or loop by the number of live locals.

Range induction uses the reusable `IntSuccessorBelow` instruction. Its operands
carry the exact `current < end` comparison result and upper bound. Independent
validation requires the comparison's true edge to dominate the instruction,
which proves `current + 1` is representable for any signed `Int` upper bound.
LLVM then emits `add nsw` without an overflow edge. The validator and emitter
do not recognize a for-loop, Fibonacci, or another exact MIR shape.

The source root boundary and LCIR artifact boundary intentionally differ. A
run root has no value, type, witness, or receiver inputs and returns `Unit`. A
source test root has no inputs and returns `Unit` or `Result[Unit, E]`; the
current scalar catalog cannot represent the latter, so it selects
whole-artifact `Unsupported(SignatureType)`. A completed LCIR
`CheckedArtifact` therefore retains the narrower zero-parameter `Unit` root
signature required by its independent validator.

A function contains:

- an `InstanceId`, stable name, source MIR function origin, signature, and
  `Effects` value;
- explicit basic blocks with typed block parameters;
- a dense instruction table and typed SSA values;
- exactly one terminator per completed block.

`InstancePlan` is the single source of callable identity. It is a dense,
deterministic table from each `InstanceId` to an `InstanceKey`. A key contains
the source MIR `FunctionId`, ordered type arguments, and ordered witness
arguments; witness arguments distinguish concrete witnesses, witness
parameters, and owned nested applications. A `Function` stores its
`InstanceId`, not a duplicate key. Its source function in `Origin` is retained
only as provenance, and validation requires it to equal the key's source.
Roots, declarations, direct calls, invokes, and effect analysis consequently
refer to planned instances rather than rebuilding a bare
`FunctionId -> InstanceId` map.

The current source lowerer creates only `InstanceKey::monomorphic(source)`, so
every produced key has empty type and witness arguments. This foundation does
not instantiate a generic function. Reachable generic source still produces
`Unsupported` during whole-artifact classification and selects the complete
legacy route atomically. Explicit builders can construct distinct keys to test
planning and validation, but that API is not a claim of generic lowering.

One public `INSTANCE_KEY_STRUCTURE_BUDGET` limits the combined nested type and
witness structure of a key to 256 nodes. Builders report
`InstanceKeyStructureBudget` before admitting an oversized key, and the
independent validator reports `LcirInstanceKeyStructureBudget` for malformed
unchecked input. Structure validation, canonical key encoding, and text output
use bounded iterative traversal instead of recursive descent. The validator
also checks the plan's program brand, dense order, one-to-one length with the
function table, key uniqueness, source-provenance agreement, and every callable
reference.

`BlockId`, `InstructionId`, and `ValueId` are local to one `InstanceId` and
carry that owner in their identity. Entry block parameters correspond to
function parameters. Other block parameters carry values across CFG edges.
All global IDs also carry a private, generative program identity. IDs printed
as the same `i0`, `t0`, or `r0` in separately built programs are not equal and
cannot be used across builders; the private identity is omitted from dumps and
diagnostics so textual output remains reproducible.

The current instruction set is deliberately small:

- `Unit`, `Bool`, `Int`, and bit-exact `Float` constants;
- Boolean negation and equality comparisons;
- floating-point negation;
- floating-point add, subtract, multiply, and divide;
- signed integer comparisons;
- a proof-carrying signed successor below an `Int` upper bound;
- explicitly ordered or unordered floating-point comparisons;
- direct calls to infallible scalar functions.

The current terminators include jump, conditional branch, return, terminal
fault, checked integer negate/add/subtract/multiply/divide, assertion,
fallible `invoke`, and `resume_fault`. A checked operation or invoke has a
`ResultTarget`: the result exists only as destination parameter zero on the
normal edge, followed by separately forwarded arguments. Its `UnwindTarget`
has no ordinary fault value and is entered with the source fault active. This
shape makes it impossible to use an operation result on its fault edge.

Fault state is part of CFG validity. Entry is inactive; ordinary and result
edges preserve their source state; unwind edges make the destination active.
An active path cannot return or originate another terminal fault and must end
in `resume_fault`. Fallible cleanup is still allowed while active. A successful
cleanup operation preserves the primary fault on its normal edge; a later
cleanup fault is suppressed, leaves the first fault primary, and continues on
an active unwind edge so remaining cleanup can run. This is the LCIR form of
the language's deterministic cleanup policy, not a choice left to LLVM.

Aggregates, managed values, dynamic dispatch, cleanup registration and
ordering, and coroutine control flow are not implemented. The current CFG can
represent the scalar operations and fault-state transitions which a later
cleanup lowering will use.

`Origin` records a source MIR function, optional MIR expression, and source
span for each function, instruction, and terminator. There is no inlining
provenance model yet.

## Validation boundary

The validator reports independently discoverable `ValidationErrors`; it does
not repair a malformed program. Current checks include:

- canonical representation tables and dense identities;
- a branded, dense, unique, structurally bounded instance plan whose entries
  agree with function origins and all callable references;
- valid function, block, instruction, value, and value-type references;
- entry parameters matching the function signature;
- no CFG predecessor for the entry block;
- one terminator per block and a valid instruction schedule;
- instruction result shapes and operand types;
- direct-call and invoke arity, types, result types, and exact callee effects;
- edge argument arity and types;
- implicit result parameter shape and type;
- return types and operation-specific fault-effect requirements;
- the exact minimal `MAY_FAULT` closure across the complete call graph;
- consistent inactive or active fault state at every block, including
  `resume_fault` and terminal-boundary rules;
- function ownership for local identities and source origins;
- no duplicate successor from one terminator, except the two logical arms of a
  conditional branch may select one destination;
- no `Uninhabited` signature or SSA value;
- reachable blocks, dominance, and use-after-definition rules.

When both branch arms carry the same arguments, LLVM emission collapses them to
one unconditional edge. When their arguments differ, the emitter creates two
physical edge blocks so each phi input has a unique LLVM predecessor. Ordinary
distinct-target branches remain direct.

These checks apply both to explicit clients and to the whole-artifact scalar
lowerer. The production automatic route consumes only the resulting checked
artifact when the complete reachable graph is supported. Source contracts
remain `Unsupported`: the generic
`ContractFailed` code does not yet carry category, user code, contract span, or
blame span, so it cannot replace production contract diagnostics.

## Text dump

`dump_program`, `write_program`, and `write_program_with_options` traverse a
`CheckedProgram`'s dense tables in their stored insertion order. Repeatedly
dumping the same `CheckedProgram` with the same options produces identical
text. Origins are omitted by default and can be included explicitly.

The dump is not canonical across independently constructed programs. Changing
function, block, parameter, or instruction insertion order may change IDs and
text even when the graphs are otherwise equivalent. The `lcir 1` text includes
the dense instance plan and complete instance keys. It is compiler-private and
has no compatibility or serialization guarantee.

## Repository evidence

The crate's focused tests cover source-root selection, recursive graph closure,
stable source-graph serialization and errors, branded artifact roots and root
signatures, distinct type/witness instance keys, dense-plan and
structural-budget validation, artifact identity and invalidation inputs, the
scalar representation catalog, target pointer-width validation, block-parameter
joins, loop backedges, pure scalar operations,
infallible direct calls, fallible invokes, edge-defined checked results, active
cleanup paths, recursive effect closure, stable fallible dumps, optional
origins, malformed SSA programs, and source-to-MIR-to-LCIR classification and
dumps for structurally different recursive and iterative Fibonacci programs.
Structural regressions cover thousands of live locals and identity branches,
bounded persistent-map allocation, and sparse-map reference differentials.
LLVM-side tests additionally cover typed ABIs, block insertion order independent
of dominance order, same-target edge normalization, exact scalar predicates,
checked arithmetic, proved successors, first-primary fault suppression, fatal
runtime setup failures, ordered tests, atomic automatic/legacy route selection,
route-separated identity, object-cache behavior, linking, execution, and
verifier/optimization gates on Linux and macOS. The platform-independent
Windows CI job checks, lints, tests, and builds `loom-codegen-ir` without
claiming a Windows LLVM backend.
