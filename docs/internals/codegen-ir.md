# Code generation IR

`loom-codegen-ir` owns two code-generation boundaries. Its source-graph module
selects checked-MIR function roots and computes the closed-world source graph
used by production native compilation. Separately, its LCIR foundation
provides target-aware scalar representations, typed SSA data structures,
builders, an independent validator, and a textual dump for tests and review.

The typed LCIR boundary is not connected to MIR lowering, the production LLVM
emitter, object emission, or the runtime. Production native compilation uses
the source graph from this crate but still lowers its reachable checked MIR
directly through `loom-codegen-llvm`'s legacy implementation. The accepted
LCIR integration and migration design is in the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md).

LCIR is compiler-private and target-specific. It is not a source IR, a public
artifact format, or a stable native ABI.

## Checked-MIR source graph

`SourceRoots` contains MIR `FunctionId` values selected for one command.
`analyze_source_reachability` closes direct calls, constructed witnesses,
dynamic requirement slots, and builtins into a deterministic
`ReachableSourceGraph`. These names deliberately include “source”: future
lowered artifact roots use LCIR `InstanceId` values and are a different graph.
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
which owns both the checked program and privately checked roots. This artifact
boundary is not connected to the production emitter yet.

`artifact_identity` and `write_artifact_identity` expose a deterministic,
compiler-private identity for that complete checked artifact. Schema 1 carries
the `typed-lcir-whole-artifact` route tag, artifact kind, ordered run or test
roots, and the canonical LCIR dump with origins enabled. The payload therefore
includes the target and representation plan, checked functions and control
flow, operations, and complete function, instruction, and terminator origins.
The dump uses explicit enum spellings and string escaping rather than Rust
`Debug`. Dense numeric IDs are content, but the process-local generative
`ProgramBrand` is deliberately excluded, so independently built isomorphic
artifacts have the same identity. A future LLVM object route can hash this
value together with its backend, target-machine, optimization, and runtime
identities; the production fingerprint does not consume it yet.

A function contains:

- an `InstanceId`, stable name, source MIR function origin, signature, and
  `Effects` value;
- explicit basic blocks with typed block parameters;
- a dense instruction table and typed SSA values;
- exactly one terminator per completed block.

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
- no duplicate successor from one terminator;
- no `Uninhabited` signature or SSA value;
- reachable blocks, dominance, and use-after-definition rules.

These checks apply only when clients construct LCIR explicitly. No current
production compiler stage constructs such a program.

## Text dump

`dump_program`, `write_program`, and `write_program_with_options` traverse a
`CheckedProgram`'s dense tables in their stored insertion order. Repeatedly
dumping the same `CheckedProgram` with the same options produces identical
text. Origins are omitted by default and can be included explicitly.

The dump is not canonical across independently constructed programs. Changing
function, block, parameter, or instruction insertion order may change IDs and
text even when the graphs are otherwise equivalent. The `lcir 0` text is
compiler-private and has no compatibility or serialization guarantee.

## Repository evidence

The crate's focused tests cover source-root selection, recursive graph closure,
stable source-graph serialization and errors, branded artifact roots and root
signatures, artifact identity and invalidation inputs, the scalar
representation catalog, target pointer-width validation, block-parameter
joins, loop backedges, pure scalar operations,
infallible direct calls, fallible invokes, edge-defined checked results, active
cleanup paths, recursive effect closure, stable fallible dumps, optional
origins, and malformed SSA programs. The
platform-independent Windows CI job checks, lints, tests, and builds this crate
without claiming a Windows LLVM backend.
