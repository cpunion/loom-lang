# Reachability and dead-code elimination

Executable artifacts are closed-world. The frontend checks the complete
resolved source graph, then the selected backend retains only definitions
needed by the command roots. Dead-code elimination never hides a source error.

The implementation lives in the `source_graph` module of `loom-codegen-ir`,
before LLVM emission. `SourceRoots` and `ReachableSourceGraph` contain
checked-MIR identities; they are intentionally distinct from the LCIR
`InstanceId` roots stored in an independently validated `CheckedArtifact`. The
production LLVM boundary maps structured `GraphError` values into native-backend
diagnostics. Source root selection and closure accept only
`loom_mir::CheckedProgram`; raw MIR cannot enter this production pass, and the
automatic native router either lowers the complete reachable typed artifact
to independently checked LCIR or stores this exact source graph for one
whole-artifact checked-MIR emission.

## Interpreted executable closure

An interpreter build uses the same selected-export source graph as its
executable seed, but a `.loomi` stores checked MIR rather than emitted machine
code. It therefore adds a serialization closure before encoding. Every global
identity mentioned anywhere in a retained function, type, contract, concept,
requirement, witness, cleanup action, or coroutine schema is retained to a
fixed point, including syntax after a `return` or diverging expression.
Retained witnesses keep the complete method table required by the independent
MIR validator. Compiler-known resource concept metadata remains present, while
marker witnesses whose concrete schemas cannot affect retained values stay
dead.

The closure preserves original relative definition order, remaps all five
global identity domains to dense indices, keeps only the selected export,
clears test roots, and runs complete MIR validation again. Encoding and
decoding then apply the ordinary executable artifact profile. A generic
checked-MIR cache entry is never narrowed: it retains all exports and checked
definitions so a later command can select another entry without recompiling
the frontend. Consequently, changing an unrelated dead body does not change
the final `.loomi` bytes, while changing a live definition does.

For the LCIR route, source reachability is followed by a second, more precise
closure over concrete callable instances. It starts from the selected exported
run function or every test root, substitutes exact type and static witness
arguments at each executable direct call, and deduplicates the resulting
instances across all roots. Generic declarations are not roots by themselves.
The typed LLVM emitter therefore declares only concrete signatures required by
the selected artifact; it never emits a universal generic body.

This exact closure also selects one direct `Text` representation for the whole
artifact. If no reachable instance concatenates/selects Text or places Text in
a product, closed sum, or transparent/refined carrier, `TextLiteral` is the
only producer and every Text uses `ImmortalText`. Run and test roots accept no
arguments, all LCIR source
callables have internal linkage, and a value passed through locals, block
parameters, direct calls, returns, or a concrete generic instance must
therefore originate in an immortal literal in the same artifact.

If any reachable instance uses `Text.concat`, `Text.get`, or a Text-bearing
product, sum, or transparent/refined carrier,
every Text in the closed artifact instead uses `ManagedPointer`, including
compiler-emitted literals. This is a Text provenance mode, not the product
representation: each aggregate remains an unboxed exact SSA value. Concat/get's
exact `MAY_COLLECT | NEEDS_RUNTIME` effect propagates through the reachable call
graph, and generated typed root maps expand only live aggregate SSA values to
their deterministic managed leaves. An unreachable concat or Text-bearing
product, sum, or transparent/refined carrier cannot change representation or
route selection. An established
transparent/refined carrier reuses its base representation and root plan.
Other unsupported dynamic Text producers still change the complete reachable
artifact to the checked-MIR route before LCIR construction.

## Roots

- A binary build/run/debug has the selected exported function as its root.
- A test build has all MIR test functions as roots.
- An empty test suite has no roots and emits an empty successful harness.
- A portable library build has no MIR or code-generation roots. Its version 4
  artifact preserves the resolved package sources and canonical public
  interfaces; a consuming binary or test later compiles those sources and
  applies its own closed-world reachability and DCE.

Root identity is part of the native-object cache key.

## Graph closure

Reachability performs a work-list traversal over checked MIR. It records:

- reachable functions;
- reachable witness instances;
- reachable builtins;
- the requirement slots used on each reachable witness.

Direct and inherent calls add concrete function edges. Static concept calls
add the selected witness method. Witness construction, coercion, and proof
passing make a witness live. Dynamic calls add a concept requirement edge;
that edge closes only over witnesses already made live by reachable code.

Straight-line witness flow through locals and tuple construction can keep a
concrete witness set. When control flow, projection, or mutation loses that
proof, analysis conservatively treats the value as unknown. Conservatism may
retain extra live witness methods but cannot remove a possible target.
Scanning follows executable evaluation order: a return or diverging operand
stops its dead suffix, conditional and short-circuit paths merge only their
continuing witness states, and a range retains its zero-iteration path. A call
mentioned only in unreachable MIR therefore cannot enlarge the artifact
closure.

Witness facts use function-local persistent sparse radix roots. Forking a
control-flow path copies one root, updates copy only the changed local's bounded
radix path, and joins skip pointer-identical subtries. Missing facts have the
single conservative meaning “unknown” and are not stored. Consequently, a
sequence of identity branches does not repeatedly clone or scan every live
local; work follows executable syntax plus witness facts that actually change.

The traversal repeats until neither functions nor witnesses grow, because a
newly retained witness method can itself make more calls and witnesses live.
Missing references in checked MIR are backend defects.

The LCIR instance traversal applies the same executable-order rule. A call
after a return or diverging expression cannot create an instance. Exact
recursive calls close back onto one key. Recursion that changes the key returns
`NonRegularGenericRecursion`; exhausting the finite instance, edge, or key-
structure budget returns `LcirLoweringProgramTooLarge`. Both fail during
planning, before any partial LCIR is allocated, and neither may select the
checked-MIR fallback. A generic function that is not reached cannot consume
those budgets or change route selection.
Concrete signature and expression substitution shares the key-structure bound.
Only substitution growth reports `LcirLoweringProgramTooLarge`; an already-wide
source type remains a direct-LCIR coverage decision.

For a concrete `dyn C` view, LCIR additionally groups executable conversion
producers by the exact concept and associated-type bindings. One closed
instantiated proof is erased to its concrete representation and every used
requirement contributes one ordinary direct method edge. Generic and
conditional conformances participate when their concrete types and prerequisite
proofs are closed. Two or more exact proofs form an ordered finite catalog. The
traversal then contributes one direct method edge per candidate for each
requirement slot that is actually called; unused slots and unrelated static
conformances remain dead. LLVM receives a compiler-private finite tag switch,
not an indirect call or witness table. A reachable concrete dynamic use with no
exact producer in the closed catalog is an invalid program. Artifact closure
reports `MissingDynamicConceptWitness`; it never emits a `SupportReport` or
selects checked-MIR fallback. The compiler never guesses a target or consults
all declared conformances.

The compiler computes this set as a least fixed point. It starts with root,
direct-call, and static-dispatch instances while the dynamic catalog is empty,
collects conversion producers in only those exact instances, and then adds the
corresponding direct dynamic-method instances. Repeating those two steps admits
producers reached by real dynamic calls without letting a conservatively seen
or prerequisite-only witness method keep itself alive through a producer in
its own body. An open producer in an unreachable function or unreachable generic
instance is never scanned and cannot produce this diagnostic or change the
artifact route.

View discovery walks both reachable expression/signature types and their
bounded concrete record, enum, and refined schemas. A unique candidate is
substituted recursively through product fields, sum payloads, generic
arguments, and List elements before the direct aggregate closure is planned.
A finite catalog instead remains one managed leaf in those shapes, and a List
descriptor records one managed pointer per element. Both paths discover views
that are stored but never projected and re-run ordinary by-value cycle checks.
Raw concrete and uniquely erased uses converge on the same canonical LCIR
value type rather than creating layout-compatible duplicates.

## Why unused conformances stay dead

Declaring `impl C for A` is not by itself a native edge. Code must reach a proof
or dynamic value that carries that conformance. A `dyn C` call therefore
selects from live witnesses, not from every implementation in the source
graph.

Loom does not provide a runtime `any -> dyn C` conformance lookup. Such a lookup
would require open-world type/conformance metadata and would make a large
candidate set conservatively reachable. Instead, conversion to a dynamic view
carries an already selected witness. Derived dynamic conversions preserve
explicit proof flow and do not search a global registry.

This closed-world rule is both a semantic boundary and a DCE property. Adding
reflection, plugins, dynamic loading, or a stable open-world witness ABI would
require a new root model rather than an exception in this pass.

## Emission and LLVM DCE

The emitter declares and defines only the reachable function set and only live
method slots for each witness. The selected development and release pass
pipelines both finish with `globaldce`, which removes backend-introduced
wrappers, globals, and helpers that LLVM proves unused.

Unique-witness LCIR erasure removes the dispatch representation before LLVM.
Only requirement slots actually called in the reachable graph become direct
method instances; other methods on the selected conformance and every method
on an unconstructed conformance are absent before backend DCE runs.

The native-object fingerprint contains the reachability result and the bodies
of reachable functions, not every private dead body. Consequently, an edit to
an unreachable private function can leave an existing object cache entry
valid.

## Testing expectations

Reachability changes need tests for:

- direct and recursive call closure;
- regular generic recursion, nonregular rejection, and planning budgets;
- literal-only versus reachable-concat artifact-wide Text representation, plus
  an unreachable concat that does not change route selection;
- duplicate concrete instances shared across calls and test roots;
- deterministic instance order and artifact identity;
- static and dynamic concept dispatch;
- a declared but never constructed conformance remaining absent;
- witness flow narrowing and conservative fallback;
- persistent witness-flow joins against a reference model and large identity
  branch sequences;
- unused requirement slots remaining absent;
- both development and release objects;
- object-cache identity after dead and live edits.

Interpreted closure tests additionally cover dense remapping in every global
identity domain, references serialized after `return`, complete retained
witness tables, dead resource-marker conformances, one selected export, stable
bytes after a dead edit, artifact round trips, and reuse of the unchanged full
checked-MIR cache for another export.

Reachability is not allowed to hide frontend errors: a dead function is still
parsed, resolved, and type-checked.
