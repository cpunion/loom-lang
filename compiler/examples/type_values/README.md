# Compile-time type values

```sh
target/loom check compiler/examples/type_values
target/loom test compiler/examples/type_values
target/loom run compiler/examples/type_values
```

The keyword `type` names compile-time type identity, not a runtime tag or a
reflection descriptor. Ordinary pure functions can accept and return it, store
it in compile-time data, and select it with loops and conditionals.
`std.meta.of[T]()` also accepts tuple/function/dynamic type syntax.

An immutable binding of a computed type can supply local annotations, generic
arguments and visible nominal constructors. `comptime { ... }` can also appear
directly in local type annotations and explicit generic arguments. Checks and
visibility still apply to the selected concrete type. No type registry, erased
conversion or type-producing helper enters native code.

Generic type construction needs an explicit static boundary: `std.meta.list`
takes a `comptime` type argument. A type-valued local during evaluation is not
itself a generic parameter or a new declaration; declaration signatures continue
to use the ordinary `[T]` parameter mechanism. Runtime values cannot select
types or be captured by a `comptime` block.
