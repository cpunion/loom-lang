# Tuple constraints

```sh
target/loom test compiler/examples/tuple_constraints
target/loom run compiler/examples/tuple_constraints
```

Tuple predicates use ordinary numeric projections. Proved construction returns
the constrained tuple directly; unknown inputs return `Result`, and invalid
constants reject. Projections and destructuring weaken the immutable outer tuple
without evaluating its source again.

Unobserved List siblings remain shared and mutable. A tuple constraint cannot
observe their contents or length. The example also covers CTFE, generic copies,
managed List elements, closure captures and a real async wait.
