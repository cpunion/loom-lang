# Boolean constraints

```sh
target/loom test compiler/examples/bool_constraints
target/loom run compiler/examples/bool_constraints
```

`Confirmed(true)` constructs the refined value directly; a known false constant
rejects. An unknown Bool retains checked `Result` construction. The example
combines type guarantees, pure compile-time evaluation, weakening, generic copies,
tuples, Lists and closure captures. A refined Bool remains Boolean, not an integer.
