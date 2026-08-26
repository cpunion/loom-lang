# Compiler pipeline

Loom is a compiler with two terminal backends, not an interpreter wrapped
around a parser. The interpreter and LLVM backend share every frontend stage
through validated MIR.

```text
project input
  -> package and source discovery
  -> lossless lexing and parsing
  -> HIR construction and name resolution
  -> static types, contracts, concepts, and witness proofs
  -> typed HIR
  -> total MIR lowering
  -> MIR validation
  -> interpreter
     or
     root selection -> reachability -> LLVM IR -> object -> linker
```

This diagram is the current production pipeline. `loom-codegen-ir` owns the
checked-MIR root and reachability graph used by that pipeline. The same crate
can build, validate, and dump hand-built scalar LCIR, but no MIR lowering or
production target emitter consumes its LCIR `CheckedProgram` yet. See
[Code generation IR](codegen-ir.md) for the implemented foundation and the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md) for the accepted
migration design.

## Project and source discovery

`loom-driver` resolves a manifest project into a closed graph before loading
source. Stable source paths, package identities, dependency aliases, selected
features, targets, and the lockfile state are part of the project snapshot.
Standalone file or directory inputs use a synthetic package identity and have
no manifest features.

All source files in the selected graph are parsed and checked. Native
reachability is a later code-generation concern; it never suppresses a type or
contract diagnostic in an otherwise dead function.

## Syntax

`loom-syntax` preserves the original token stream and source spans. Parser
recovery produces diagnostics without inventing an executable program.
Formatting uses the same syntax representation. Nesting is bounded so source,
artifact, and fuzz inputs cannot force unbounded recursive traversal.

## HIR and semantic analysis

Syntax is lowered into source-independent HIR identities. `loom-sema` then
builds:

- module and namespace maps;
- declaration and body types;
- place and mutability facts;
- contract and refined-value checks;
- concept requirements, conformances, associated bindings, and witness proofs;
- obligations governing Task and scoped resources.

Semantic analysis stores facts in side tables rather than teaching the backend
to reinterpret surface syntax. Diagnostics retain source spans through the HIR
and MIR boundaries.

`AnalysisHost` owns a long-lived project and optional source overlays. A
snapshot provides a consistent source map, project graph, diagnostics,
semantic query statistics, and—only when all required stages succeed—an
executable MIR program. The CLI and LSP use this shared driver model.

## Lowering and checked MIR

`loom-lowering` converts typed HIR into explicit executable operations:
construction modes, contracts, calls, witness dispatch, cleanup, Task
suspension, and builtins. Lowering is total: it either produces a complete
`loom_mir::CheckedProgram` or reports an unavailable stage. There is no
partially executable fallback and no raw `Program` result from the lowering
API.

Expression identities are canonicalized per function. The independent MIR
validator then checks indices, types, dataflow, contracts, witnesses, task
liveness, and operation shapes. The driver snapshot and persistent checked-MIR
cache retain that wrapper. The interpreter, portable-library encoder,
interpreted-artifact encoder, source-reachability analysis, native-object
fingerprint, and LLVM emission entry points accept only
`loom_mir::CheckedProgram`.

## Command roots

Frontend checking and backend roots are intentionally different:

- `check` validates the full resolved source graph and emits no object.
- a binary `build`, `run`, or `debug` selects one public entry;
- `test` selects every MIR test in the chosen test graph;
- an empty test set produces a successful empty harness;
- a library target serializes portable checked MIR rather than selecting an
  executable root.

For native code, root selection is followed by closed-world reachability.
Direct calls, static witness calls, dynamic witness slots, builtins, and async
descriptors all contribute edges. See
[Reachability and dead-code elimination](reachability-and-dce.md).

## Backend boundaries

The interpreter executes MIR deterministically and provides an independent
semantic oracle for end-to-end tests. The production LLVM backend receives the
source graph computed by `loom-codegen-ir`, computes the target identity, and
lowers only reachable checked MIR through its legacy
universal-value implementation and private native specializations, verifies
LLVM IR, optimizes it, verifies again, and emits a relocatable object. Linking
is a separate step.

`loom-codegen-ir::CheckedProgram` is currently an independent LCIR library
boundary, not part of either terminal backend. It is distinct from the
production `loom_mir::CheckedProgram` consumed by the crate's source graph and
both terminal backends. Whole-artifact LCIR selection and an LCIR-to-LLVM
emitter remain implementation work tracked by the
[accepted RFC](../rfcs/typed-codegen-ir.md).

Source diagnostics exit before either backend executes. Errors discovered
after checked MIR—missing MIR references, LLVM verifier failures, or malformed
compiler-generated ABI metadata—are reported as defects, not source errors.
