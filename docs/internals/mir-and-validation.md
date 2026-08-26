# MIR and validation

Loom MIR is the typed executable contract between semantic analysis and every
execution backend. It is compiler-private, but its serialized forms are
versioned because caches and portable artifacts cross process boundaries.

## Program model

A MIR `Program` contains dense tables for:

- nominal types, record fields, enum variants, refined predicates, and
  invariants;
- concepts, associated types, requirements, and dynamic capability;
- functions, locals, expressions, contracts, suspension points, and cleanup;
- conformance witnesses and requirement-to-function method maps;
- exported names and test roots;
- compiler-known prelude identities.

MIR types include primitive values, tuples, lists, nominal instantiations,
generic parameters, associated projections, `Task`, `TaskOutcome`, dynamic
views, and the internal error type. Surface conveniences are gone: a call has
an explicit target and arguments, a construction has an explicit checking
mode, and dispatch carries explicit proof references.

Expression IDs form a dense, deterministic preorder within each function.
They are not source AST identities.

## Validation boundary

`Program::into_checked` consumes unchecked MIR and returns `CheckedProgram`
only after all validators succeed. The interpreter, LLVM pipeline, checked-MIR
cache, `.loomi` loader, and `.loomlib` loader use this boundary. Typed-HIR
lowering returns the wrapper directly; driver snapshots and cache hits retain
it; source reachability, both execution backends, native-object identity and
emission, interpreted-artifact encoding, and portable-library encoding require
it in their public APIs. Decoding remains an untrusted boundary and validates
the embedded raw program before returning the wrapper.

Validation covers:

- dense table indices and all referenced type/function/concept/requirement,
  witness, variant, local, and expression identities;
- control-flow-sensitive local initialization and moves, including
  short-circuit joins and preservation of every available loop-entry local on
  continuing range backedges;
- projections, mutability, place legality, and call-scoped loan state;
- expression and statement type equality;
- call, receiver, generic proof, witness, record, variant, pattern, and builtin
  arity/shape;
- contract schemas and the types visible to each contract arm;
- concept definitions, requirement schemas, witnesses, associated bindings,
  prerequisites, and method slots;
- error-type confinement and source nesting limits;
- Task obligations, suspension shapes, and exact live-slot maps.

The validator accumulates independently discoverable failures with stable
structural paths. It does not guess intent or repair malformed values.

## Suspension liveness

Async lowering records state-machine suspension points and live Task-frame
slots. Liveness is computed backward over MIR control flow. The validator
recomputes the expected set and rejects a descriptor that omits a live value or
keeps an inconsistent slot shape. LLVM and the moving GC therefore consume the
same exact state/slot contract.

This is important for both correctness and memory safety: a managed value live
across `.await` must be traced, while a dead value must not be interpreted as a
root merely because storage still exists.

## Artifact encoding

The interpreted MIR envelope currently uses:

- format `loom.interpreted-mir`;
- artifact version `17`;
- Loom language version `0.3`.

Executable `.loomi` artifacts additionally bind one validated exported entry.
The decoder checks the envelope, format and language versions, nesting bounds,
numeric encodings, the entry, and the complete MIR program.

Portable library artifacts use a separate versioned envelope (`.loomlib`
version `1`) around checked MIR and package/public-interface metadata.

Neither serialization is a public extension API. Tools must use the project
decoder and validator rather than constructing JSON that happens to match the
current wire shape.

## Failure policy

- Invalid user source produces ordinary diagnostics before MIR is executable.
- Malformed external artifact or cache bytes are rejected or treated as a
  cache miss.
- Invalid MIR produced by the current compiler is a compiler defect.

Adding a MIR operation requires updates to serialization, validation,
interpreter semantics, liveness where applicable, LLVM lowering, cache
identity, and focused malformed-input tests.
