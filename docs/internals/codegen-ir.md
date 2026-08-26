# Code generation IR

`loom-codegen-ir` is a standalone foundation for Loom Codegen IR (LCIR). It
currently provides target-aware scalar representations, typed SSA data
structures, builders, an independent validator, and a textual dump for tests
and review.

The crate is not connected to MIR lowering, root selection, reachability, the
production LLVM emitter, object emission, or the runtime. Production native
compilation still lowers checked MIR directly through `loom-codegen-llvm`'s
legacy implementation. The accepted integration and migration design is in
the [typed code generation IR RFC](../rfcs/typed-codegen-ir.md).

LCIR is compiler-private and target-specific. It is not a source IR, a public
artifact format, or a stable native ABI.

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
- Boolean negation;
- floating-point add, subtract, multiply, and divide;
- signed integer comparisons;
- explicitly ordered or unordered floating-point comparisons;
- direct calls to infallible scalar functions.

The current terminators are jump, conditional branch, return, and terminal
fault. Checked integer arithmetic, fallible calls, edge-defined results,
cleanup, aggregates, managed values, dynamic dispatch, and coroutine control
flow are not implemented.

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
- direct-call arity, types, and infallible-callee effects;
- edge argument arity and types;
- return types and fault effects;
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

The crate's focused tests cover the scalar representation catalog, target
pointer-width validation, block-parameter joins, loop backedges, infallible
direct calls, float comparison spelling, optional origins, and malformed SSA
programs. The platform-independent Windows CI job checks, lints, tests, and
builds this crate without claiming a Windows LLVM backend.
