# Reachability and dead-code elimination

Native artifacts are closed-world. The frontend checks the complete resolved
source graph, then LLVM code generation retains only code and conformance data
reachable from the selected command roots.

The implementation lives in the `source_graph` module of `loom-codegen-ir`,
before LLVM emission. `SourceRoots` and `ReachableSourceGraph` contain
checked-MIR identities; they are intentionally distinct from the LCIR
`InstanceId` roots stored in an independently validated `CheckedArtifact`. The
production LLVM boundary maps structured `GraphError` values into native-backend
diagnostics. Source root selection and closure accept only
`loom_mir::CheckedProgram`; raw MIR cannot enter this production pass, and the
automatic native router either lowers the complete reachable typed artifact
to independently checked LCIR or stores this exact source graph for one
whole-artifact legacy emission.

For the LCIR route, source reachability is followed by a second, more precise
closure over concrete callable instances. It starts from the selected exported
run function or every test root, substitutes exact type and static witness
arguments at each executable direct call, and deduplicates the resulting
instances across all roots. Generic declarations are not roots by themselves.
The typed LLVM emitter therefore declares only concrete signatures required by
the selected artifact; it never emits a universal generic body.

This exact closure also selects one direct `Text` representation for the whole
artifact. If no reachable instance concatenates/selects Text or places Text in
a product/closed sum, `TextLiteral` is the only producer and every Text uses
`ImmortalText`. Run and test roots accept no arguments, all LCIR source
callables have internal linkage, and a value passed through locals, block
parameters, direct calls, returns, or a concrete generic instance must
therefore originate in an immortal literal in the same artifact.

If any reachable instance uses `Text.concat`, `Text.get`, or a Text-bearing
product/sum,
every Text in the closed artifact instead uses `ManagedPointer`, including
compiler-emitted literals. This is a Text provenance mode, not the product
representation: each aggregate remains an unboxed exact SSA value. Concat/get's
exact `MAY_COLLECT | NEEDS_RUNTIME` effect propagates through the reachable call
graph, and generated typed root maps expand only live aggregate SSA values to
their deterministic managed leaves. An unreachable concat or Text-bearing
product/sum cannot change representation or route selection. Other unsupported
dynamic Text producers, Text inside a transparent/refined carrier, and managed
lists still change the complete reachable artifact to the legacy route before
LCIR construction.

## Roots

- A binary build/run/debug has the selected exported function as its root.
- A test build has all MIR test functions as roots.
- An empty test suite has no roots and emits an empty successful harness.
- A portable library artifact is not reduced to one executable root; it
  preserves the package's validated public surface.

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
recursive calls close back onto one key, while recursion that changes the key
is rejected as unsupported under finite instance, edge, and key-structure
budgets. This rejection occurs during planning, before any partial LCIR is
allocated. A generic function that is not reached cannot consume those budgets
or change direct-versus-legacy route selection.

For a concrete `dyn C` view, LCIR additionally groups the reachable witnesses
by the exact concept and associated-type bindings. If that set contains one
closed nongeneric conformance, the instance traversal resolves every dynamic
requirement through that witness and records the resulting ordinary direct
method edge. The view itself uses the concrete value representation, so LLVM
receives no dispatch table, indirect call, type tag, or witness pointer. A
missing, open, or competing witness set is a structured unsupported site; the
compiler never guesses a target or consults all declared conformances.

View discovery walks both reachable expression/signature types and their
bounded concrete record, enum, and refined schemas. The selected concrete type
is substituted recursively through product fields, sum payloads, generic
arguments, and List elements before the direct aggregate closure is planned.
This catches a view stored but never projected, keeps the List repeated
descriptor identical to its concrete element descriptor, and re-runs ordinary
by-value cycle rejection after erasure. Raw concrete and erased uses converge
on the same canonical LCIR value type rather than creating layout-compatible
duplicates.

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

Reachability is not allowed to hide frontend errors: a dead function is still
parsed, resolved, and type-checked.
