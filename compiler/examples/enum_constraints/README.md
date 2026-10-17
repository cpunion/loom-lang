# Enum constraints

```sh
target/loom test compiler/examples/enum_constraints
target/loom run compiler/examples/enum_constraints
```

Ordinary pure predicates can constrain enum tags and immutable payload values.
Proved constructors return the constrained enum directly; unknown inputs retain
checked `Result` construction. Match bindings preserve the original nominal type
and ordinary base-domain coverage.

`?` can read a constrained Result. In a constrained-Result-returning function,
its automatic error return must also satisfy the destination constraint. Reusing
an identical incoming value preserves its guarantee; otherwise a guarded static
proof is required. Unknown or false obligations reject the build, rather than
adding a hidden runtime check or bypassing the constraint.

The example combines CTFE, closures, Lists, moving GC and real async waits.
Unobserved List payloads stay shared and mutable; constraints cannot observe
their contents or length. Scoped resources and one-shot Tasks cannot be stored
inside a constrained enum.
