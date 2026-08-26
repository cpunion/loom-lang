# Reachability and dead-code elimination

Native artifacts are closed-world. The frontend checks the complete resolved
source graph, then LLVM code generation retains only code and conformance data
reachable from the selected command roots.

The implementation lives in the `source_graph` module of `loom-codegen-ir`,
before LLVM emission. `SourceRoots` and `ReachableSourceGraph` contain
checked-MIR identities; they are intentionally distinct from future LCIR
instance roots. The LLVM boundary maps structured `GraphError` values into
native-backend diagnostics. Root selection and closure accept only
`loom_mir::CheckedProgram`; raw MIR cannot enter this production pass.

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

The traversal repeats until neither functions nor witnesses grow, because a
newly retained witness method can itself make more calls and witnesses live.
Missing references in checked MIR are backend defects.

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

The native-object fingerprint contains the reachability result and the bodies
of reachable functions, not every private dead body. Consequently, an edit to
an unreachable private function can leave an existing object cache entry
valid.

## Testing expectations

Reachability changes need tests for:

- direct and recursive call closure;
- static and dynamic concept dispatch;
- a declared but never constructed conformance remaining absent;
- witness flow narrowing and conservative fallback;
- unused requirement slots remaining absent;
- both development and release objects;
- object-cache identity after dead and live edits.

Reachability is not allowed to hide frontend errors: a dead function is still
parsed, resolved, and type-checked.
