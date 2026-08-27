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

## Construction proof provenance

Semantic analysis can mark a refined-type or invariant-bearing record
construction as `Proven`. That mark is a process-local compiler conclusion,
not a portable proof certificate. Fresh source keeps the direct nominal result
and emits no predicate or invariant check.

Portable checked-MIR serialization writes `Recheck`. Decoding also changes a
forged `Proven` spelling to `Recheck` before MIR validation. This rule applies
to standalone `.loomi` envelopes and the checked-MIR payload inside
`.loomlib`.

`Recheck` retains the direct nominal result type; it is not the source-facing
`Result[T, ConstraintError]` produced by an ordinary runtime-checked
construction. The interpreter and legacy LLVM route replay the embedded
predicate or invariant exactly once, using a private candidate and publishing
the nominal destination only after acceptance. Success preserves source
behavior. Failure raises the canonical `ArtifactProofRejected` `RuntimeFault`;
it cannot become a source `Result` or a nominal value. Direct calls, `.await`,
and `Task.all` propagate it normally. `Task.settled` and `Task.race` observe a
faulted child through the same `TaskFault` terminal-state rules as every other
child fault. Only OOM is a process-level exception. Typed LCIR currently
rejects `Recheck` and atomically selects the legacy route for the whole
artifact.

The persistent compiler cache does not turn a cache hit into a replay build.
Proof-bearing checked MIR and typed semantic state are not published, and a
forged cached proof disposition is rejected as a miss. A later process rebuilds
those layers from the exact source, obtains the same process-local `Proven`
conclusion, and therefore preserves cold/warm diagnostics, route selection and
check elimination. In-process semantic reuse remains available because it has
not crossed a wire trust boundary.

This rule prevents the construction disposition alone from bypassing the
predicate or invariant embedded in the same artifact. It does not authenticate
the source, type definition, or artifact as a whole. Registry and lockfile
checksums, authenticated distribution, and the user's trust policy establish
that separate boundary. A local cache digest detects corruption; it is not a
credential or a signature against an attacker with the same filesystem
authority.

A future serialized proof format may avoid replay only if it carries a
structured certificate that the decoder can independently validate. A boolean
or enum disposition is not such a certificate.

Within one fresh compilation, validation still checks that `Proven` appears
only at a matching refined predicate or record-invariant construction and that
all operand/result types agree. It does not rerun semantic proof search. Thus
checked MIR is a structural and execution-safety boundary; it is not an
authenticated proof-provenance format.

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
- artifact version `19`;
- Loom language version `0.3`.

Executable `.loomi` artifacts additionally bind one validated exported entry.
The decoder checks the envelope, format and language versions, nesting bounds,
numeric encodings, the entry, and the complete MIR program.

Portable library artifacts use a separate versioned envelope (`.loomlib`
version `1`) around checked MIR and package/public-interface metadata. Its
nested checked-MIR envelope is still version `19` and uses the construction
proof rule above.

Neither serialization is a public extension API. Tools must use the project
decoder and validator rather than constructing JSON that happens to match the
current wire shape. The wire format is not an authenticated proof interchange:
even a wire spelling of `Proven` is normalized to `Recheck`, while trust in the
artifact's source and publisher remains a separate distribution concern.

## Failure policy

- Invalid user source produces ordinary diagnostics before MIR is executable.
- Malformed external artifact or cache bytes are rejected or treated as a
  cache miss.
- Invalid MIR produced by the current compiler is a compiler defect.

Adding a MIR operation requires updates to serialization, validation,
interpreter semantics, liveness where applicable, LLVM lowering, cache
identity, and focused malformed-input tests.
