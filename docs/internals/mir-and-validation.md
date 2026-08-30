# MIR and validation

Loom MIR is the typed executable contract between semantic analysis and every
execution backend. It is compiler-private, but its checked-MIR cache and
`.loomi` serializations are versioned because they cross process boundaries.

## Program model

A MIR `Program` contains dense tables for:

- nominal types, record fields, enum variants, refined predicates, and
  invariants;
- concepts, associated types, requirements, and dynamic capability;
- functions, locals, expressions, contracts, suspension points, and cleanup;
- conformance witnesses and requirement-to-function method maps;
- exported names and test roots;
- compiler-known prelude identities.

Concept metadata retains its source package separately from its unqualified
name. Semantic analysis resolves `Dispose`, `MustScope`, and `NoSuspend` only
from the exact current compiler-owned `std` module and its `std.resource`
package. Lowering consumes those resolved `DefId` values
without reconstructing names and assigns distinct compiler-known `Dispose`,
`MustScope`, and `NoSuspend` identity tags only to those three definitions.

MIR types include primitive values, tuples, lists, nominal instantiations,
generic parameters, associated projections, `Task`, `TaskOutcome`, dynamic
views, and the internal error type. Surface conveniences are gone: a call has
an explicit target and arguments, a construction has an explicit checking
mode, and dispatch carries explicit proof references.

Expression IDs form a dense, deterministic preorder within each function.
They are not source AST identities.

`StatementKind::Scoped` preserves resource lifetime intent as a first-class MIR
operation. It contains the initializer and a statically resolved
`Dispose.dispose` proof. The cleanup is registered only after initialization
succeeds, belongs to the current lexical block, and shares one LIFO stack with
`defer`. Canonical File and Socket disposal uses the same source witness path as
every other resource.

## Validation boundary

`Program::into_checked` consumes unchecked MIR and returns `CheckedProgram`
only after all validators succeed. The interpreter, LLVM pipeline, checked-MIR
cache, and `.loomi` loader use this boundary. Typed-HIR lowering returns the
wrapper directly; driver snapshots and cache hits retain it; source
reachability, both execution backends, native-object identity and emission, and
interpreted-artifact encoding require it in their public APIs. Decoding remains
an untrusted boundary and validates the embedded raw program before returning
the wrapper. The current `.loomlib` decoder instead validates source-and-interface
structure and interfaces; its consumer creates fresh checked MIR through the
normal frontend.

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
- parameter shape: only parameter zero of a synchronous `mut self` method may
  be a mutable slot; ordinary parameters and all coroutine parameters are
  immutable slots, and coroutines cannot carry receiver metadata;
- protected-value boundaries: mutation and projected moves cannot cross a
  constrained-type predicate or record-invariant interior; non-move mutation
  may cross only the current synchronous `mut self` parameter's own top-level
  record invariant, never a constrained wrapper or nested invariant;
- contract schemas and the types visible to each contract arm;
- finite by-value layouts for record, enum, refined, tuple, nominal-argument,
  `Option`, `Result`, and `TaskOutcome` graphs;
- concept definitions, requirement schemas, witnesses, associated bindings,
  prerequisites, and method slots;
- error-type confinement and source nesting limits;
- Task obligations, suspension shapes, and exact live-slot maps;
- canonical `Dispose`, `MustScope`, and `NoSuspend` prelude identities, plus
  independent affine resource-flow and cleanup-stack validation.

For resource concepts, only a compiler-known identity tag paired with its
matching prelude id grants language semantics. Package and name metadata cannot
create an identity: even an untagged low-level concept spelled exactly
`std.resource.MustScope` remains ordinary. Once an identity is asserted,
the validator cross-checks that its tagged dense id is the unique declaration
with the expected `std.resource` package and name. It also requires the
fixed non-dynamic shape, including the exact `Dispose.dispose(mut self)`
requirement. A missing, redirected, duplicated, or cross-tagged identity is a
fail-closed resource result for all loss, escape, receiver, and place-use
checks, in addition to producing `MirConceptShape`.

The general `CheckedProgram` boundary permits all three identities and their
prelude entries to be absent together. This keeps focused low-level MIR tools
independent of the compiler-distributed library. Persistent interpreted
artifact encoding and decoding apply a stricter profile: `Dispose`,
`MustScope`, and `NoSuspend`, all three prelude ids, and the canonical Dispose
requirement must be present and valid. A completely missing trio is therefore
valid only as non-artifact checked MIR. These identity checks establish MIR
structure, not publisher authenticity; registry and distribution validation
remain responsible for artifact provenance.

Persistent typed-semantic cache entries intentionally do not carry
`CanonicalConcepts` as proof authority. On a compatible cache hit,
`analyze_reusing_bodies` resolves the identity again from the current
package-qualified HIR before MIR lowering observes the analysis.

The validator accumulates independently discoverable failures with stable
structural paths. It does not guess intent or repair malformed values.

A projected `Copy` observes only its leaf. A projected `Move` transfers that
leaf and marks the complete root local moved. Consuming the root is intentional:
checked MIR has no partial-initialization state, so later access to either the
root or a sibling is rejected until the root is assigned again. Projected
assignment reconstructs the complete value through its typed field path. A
read may cross an established constrained wrapper or record invariant, but a
move may not cross its interior. Assignment, inout, mutable interface creation,
and mutable interface reborrow may cross only the owning synchronous `mut self`
receiver's top-level record invariant. Checked MIR rejects every constrained,
external, or nested crossing as `MirInvariantShape`.

The validator carries the owning receiver's dirty state through branches by
union and requires every continuing loop backedge to reproduce the entry
state. A complete checked assignment clears the corresponding state. A source
`assert` that semantically proves the exact declared receiver invariant is
followed by `RestoreReceiverInvariant(Proven)`; this marker carries no copied
contract and restores only the current receiver. Artifact decoding changes the
marker to `Recheck`, derives the invariant from the receiver's nominal type,
and requires the receiver to remain available.

Fault paths are stricter than normal continuations. Mutable inout and mutable
interface writeback identify every enclosing invariant-bearing place. Those
places become unavailable to the active cleanup suffix on the fault edge, and
cleanup cannot copy, move, project, borrow, or call through them. A complete
checked replacement is the only operation that clears this fault state. State
joins take the union, so no branch can erase an invalidated place. Successful
mutable receiver calls keep their normal continuation clean because the callee
has completed its normal exit invariant check. This analysis is deliberately
conservative at an erased or open boundary: if the borrowed type contains a
nested invariant, or is a type parameter or associated projection, the
complete borrowed root is protected. Dynamic views are opaque boundaries:
hidden state can be observed only by dispatching an exact witness method, whose
receiver invariant is checked on entry. A receiver-restoration marker cannot
clear fault protection, and projected recovery writes retain it; only complete
checked replacement can clear it.

### Bounded recursive type analysis

Before validating executable bodies, MIR builds the finite nominal declaration
graph and rejects every strongly connected component made only of by-value
edges as `MirRecursiveValueType`. Tuple fields, nominal arguments, and
`TaskOutcome` payloads remain inline; `List`, canonical `TextMap`, `Task`, and
dynamic views are explicit indirection boundaries. This independently repeats
the frontend rule for decoded or otherwise untrusted MIR and never inserts a
hidden allocation.

Validation never expands the recursive types that remain legal through an
indirection into an unbounded concrete type tree. Resource and Task containment
are evaluated as a least fixed point over finite abstract argument states.
Value equality is evaluated as the corresponding greatest fixed point. For
example, the non-regular indirect schema
`Spiral[T] = Done(T) | Next(List[Spiral[(T, T)]])` reaches the same abstract
state for `Int` without materializing successively doubled tuple arguments.
Argument transitions still matter: a recursive edge that replaces `T` with
`File` retains the resource obligation and disables value equality.

Each recursive analysis has a 4,096-node work budget and the common nesting
limit. Pattern usefulness and exhaustiveness have an independent 4,096-step
budget, and substitutions performed for nested patterns may materialize at
most 4,096 type nodes. Exhausting a budget is conservative: validation never
claims that an arm is unreachable or a match exhaustive from incomplete work.
An explicit nested pattern whose substituted type exceeds the bound receives a
stable `MirNestingLimit` diagnostic.

## Construction proof provenance

Semantic analysis can mark a refined-type or invariant-bearing record
construction as `Proven`. That mark is a process-local compiler conclusion,
not a portable proof certificate. Fresh source keeps the direct nominal result
and emits no predicate or invariant check.

Portable `.loomi` serialization writes `Recheck`. Decoding also changes a
forged `Proven` spelling to `Recheck` before MIR validation. A `.loomlib`
carries no MIR or construction disposition: its source is analyzed again and
the consuming frontend derives a fresh `Proven` or runtime-checked construction
as appropriate.

`Recheck` retains the direct nominal result type; it is not the source-facing
`Result[T, ConstraintError]` produced by an ordinary runtime-checked
construction. The interpreter and checked-MIR LLVM route replay the embedded
predicate or invariant exactly once, using a private candidate and publishing
the nominal destination only after acceptance. Success preserves source
behavior. Failure raises the canonical `ArtifactProofRejected` `RuntimeFault`;
it cannot become a source `Result` or a nominal value. Direct calls, `.await`,
and `Task.all` propagate it normally. `Task.settled` and `Task.race` observe a
faulted child through the same `TaskFault` terminal-state rules as every other
child fault. Only OOM is a process-level exception. Typed LCIR replays
supported nongeneric predicates through an explicit `ArtifactProofRejected`
fault guard and publishes the nominal SSA value only in the accepted block;
generic or otherwise unsupported shapes atomically select the checked-MIR route.

The canonical prelude `ConstraintError` is a non-generic record with exactly
six fields, in order: `target_type Text`, `code Text`, `predicate Text`, `path
List[Text]`, `value_summary Text`, and `contract_span (Int, Int, Int)`. Checked
MIR validation rejects any different name, order, type, arity, type-parameter
count, or invariant. Its value summary is type-only and cannot reveal a scalar,
content, length, variant, collection count, or nested value.

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
- artifact version `42`;
- Loom language version `0.3`.

Generic compiler-cache envelopes carry an explicit null `entry`. Executable
`.loomi` envelopes instead bind one exported entry string. Their decoder APIs
reject the opposite envelope kind before deserializing the MIR program. The
matching decoder then checks nesting bounds, numeric encodings, the executable
entry when present, and the complete MIR program. MIR deserialization is exact:
every current field is required, unknown fields are rejected at every MIR
container, and no omitted field receives a synthesized default.

The compiler does not encode its complete cached program as an executable.
Before writing a `.loomi`, it starts from the selected export's executable
reachability, closes every reference still present in the retained serialized
definitions, and densely remaps type, function, concept, requirement, and
witness identities. This second closure deliberately includes references in a
retained body even when control-flow reachability proves that syntax cannot
execute; retaining the whole body while dropping such a reference would create
malformed MIR. The result exports only the selected entry, has no test roots,
and crosses `check_program` again before artifact encoding. The original full
checked program remains unchanged for incremental reuse and other entry
selections.

Portable library artifacts use a separate source-and-interface envelope
(`.loomlib` version `3`). It contains the resolved non-stdlib module graph,
exact Loom source text, and canonical public interfaces. It contains no checked
MIR, producer-local construction dispositions, or compiler-owned
standard-library implementation. Decoding enforces structural and byte/count
bounds, package/dependency identities, graph closure, portable unique paths,
and the format and language versions. It parses the embedded source to
recompute and compare every public-interface fingerprint. The loader reads no
more than the encoded artifact bound and derives a deterministic
per-package Merkle identity whose dependency edges include the dependency
content identity. Diamond-shaped artifact graphs can therefore deduplicate
identical packages without weakening transitive lockfile integrity. The
consumer pipeline then supplies its matching compiler-distributed standard
library and repeats parsing, type checking, proof search, lowering, and MIR
validation.
Any other portable-library format or version is rejected rather than upgraded.

Neither `.loomi` nor `.loomlib` is a public extension API. Tools must use the
corresponding project decoder rather than constructing JSON that happens to
match the current wire shape. The `.loomi` wire format is not an authenticated
proof interchange: even a wire spelling of `Proven` is normalized to
`Recheck`. A `.loomlib` stores no proof claim at all. Trust in either artifact's
source and publisher remains a separate distribution concern. The persistent
compiler checked-MIR cache uses its own versioned decoder and validation
boundary; portable-library artifacts are not checked-MIR cache entries.

## Failure policy

- Invalid user source produces ordinary diagnostics before MIR is executable.
- Malformed external artifact or cache bytes are rejected or treated as a
  cache miss.
- Invalid MIR produced by the current compiler is a compiler defect.

Adding a MIR operation requires updates to serialization, validation,
interpreter semantics, liveness where applicable, LLVM lowering, cache
identity, and focused malformed-input tests.
