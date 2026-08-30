# Compiler pipeline

Loom is a compiler with two terminal backends, not an interpreter wrapped
around a parser. The interpreter and LLVM backend share every frontend stage
through validated MIR.

```text
project input
  -> module, directory-package, and source discovery
     -> versioned compiler-distributed standard-library source
  -> lossless lexing and parsing
  -> HIR construction and name resolution
  -> static types, contracts, concepts, and witness proofs
  -> typed HIR
  -> total MIR lowering
  -> MIR validation
  -> interpreter
     or
     native route preparation
       -> exact LLVM target machine and target layout
       -> whole-artifact direct classification/lowering
          -> complete checked LCIR -> typed LLVM emitter
          or
          -> unsupported
             -> Automatic -> checked-MIR reachability -> universal-value LLVM emitter
             -> LcirOnly -> structured support-report error
       -> object cache -> linker
```

This diagram is the current production pipeline. `loom-codegen-ir` owns both
checked-MIR source reachability and whole-artifact direct MIR-to-LCIR
lowering. `loom-codegen-llvm` prepares one opaque route, target machine, and
route-specific object identity for the cache and emitter. See
[Code generation IR](codegen-ir.md) for the implemented boundary and the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md) for the remaining
migration gates.

## Project and source discovery

`loom-driver` resolves a manifest project into a closed graph before loading
source. Stable source paths, module identities, package paths, dependency aliases, selected
features, targets, and the lockfile state are part of the project snapshot.
Standalone file or directory inputs use the synthetic `<standalone>@0` module
identity and have no manifest features.

The source-backed portion of the compiler-distributed standard library is a
read-only `std` module available through a reserved direct dependency. It
is not concatenated with a root source file and it receives no privileged
frontend pass: its packages are parsed, resolved, checked, lowered, and
selected by the same reachability rules as user packages. The current module
contains the `std.int` and `std.json` parsers with their ordinary source error
values, `std.log` convenience functions over the single output boundary, and
the public resource concept declarations in `std.resource`. `Dispose`, `MustScope`, and
`NoSuspend` pass through the ordinary source pipeline, while their canonical
identity, required shapes, and lexical static rules remain compiler-enforced
and require no runtime registry. JSON formatting remains one exact typed
compiler/runtime operation; parsing has no special compiler or runtime path. See
[Core, standard library, and runtime boundary](core-library-runtime-boundary.md).

The `std` identity is the only compiler-owned module identity in language
version 0.3.

A version 3 `.loomlib` dependency enters at this same source boundary. Its
decoder validates the bounded module/package/source structure and recomputes
the stored public interfaces, then exposes the embedded files as read-only package
sources. It does not supply checked MIR, producer proof decisions, or a
standard-library implementation. The consumer injects its compiler-distributed
`std` module and compiles the resulting graph normally.

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

- package and namespace maps;
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
cache retain that wrapper. The interpreter, interpreted-artifact encoder,
source-reachability analysis, native-object fingerprint, and LLVM emission
entry points accept only `loom_mir::CheckedProgram`.

A library build must still pass the complete producer frontend before it can
publish an artifact. Its encoder receives the validated project/source
snapshot, however, and serializes source plus canonical public interfaces—not
the resulting `CheckedProgram`. The consuming frontend repeats parsing,
semantic checks and proof search, MIR lowering, and MIR validation.

## Command roots

Frontend checking and backend roots are intentionally different:

- `check` validates the full resolved source graph and emits no object.
- a binary `build`, `run`, or `debug` selects one public entry;
- `test PATH` adds one directory package's `*_test.loom` sources, while
  `test PATH/...` adds every package in the root module; dependency test
  sources are never loaded;
- an empty test set produces a successful empty harness;
- a library target packages the resolved module graph and public interfaces;
  it has no executable or code-generation root.

For native code, root selection is followed by closed-world reachability.
Direct calls, static witness calls, dynamic witness slots, builtins, and async
descriptors all contribute edges. See
[Reachability and dead-code elimination](reachability-and-dce.md).

## Backend boundaries

The interpreter executes MIR deterministically and provides an independent
semantic oracle for end-to-end tests. The production LLVM path prepares one
representation-neutral target machine, derives LCIR's pointer-width layout
from its target data, and attempts one atomic whole-artifact direct lowering
for primitive values, literal or concat-produced direct `Text` on 64-bit targets,
structural tuples, closed records, and established transparent refined values,
including eligible closed concrete enums.
A complete result retains only the independently validated `CheckedArtifact`
and uses the typed LCIR emitter. Only a valid `Unsupported` result selects the
checked-MIR source graph and universal-value emitter for the complete artifact.
Invalid roots, resource exhaustion, compiler defects, and LCIR emission
failures never select fallback.

Tooling can select `NativeRoutePolicy::LcirOnly` at the same preparation
boundary. It performs the identical whole-artifact classification but returns
`NativePreparationUnsupportedLcir` with the ordered `SupportReport` instead of
constructing a checked-MIR plan. `CheckedMirOnly` remains available for focused
checked-MIR backend validation. Route policy never changes the identity of an
otherwise identical selected object.

The prepared plan owns its `EmitOptions` and exact target machine. Cache
identity, runtime-bundle validation, optimization, and object emission reuse
that plan instead of reconstructing target or reachability state. Ordinary
`build`, `run`, `test`, and `debug` use automatic selection. A complete LCIR
artifact keeps the typed route in development debug builds; an unsupported
reachable construct selects the complete checked-MIR route exactly as it does for
the other commands. Linking remains a separate step.

Source diagnostics exit before either backend executes. Errors discovered
after checked MIR—missing MIR references, LLVM verifier failures, or malformed
compiler-generated ABI metadata—are reported as defects, not source errors.
