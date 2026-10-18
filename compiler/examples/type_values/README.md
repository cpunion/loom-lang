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
The same keyword works in explicit generic arguments (`new[type]()`) and as
the compile-time identity of `type` itself; it is still reserved as a name and
cannot become a native value or an index.
`std.meta.parameter_types(callback)` returns the ordered `List[type]` inputs of
a known function value. `std.meta.return_type(callback)` returns `Option[type]`:
`None` for an omitted result, without exposing a `Unit` type. These ordinary
Loom helpers use structural function packs and never execute the callback.
Their results stay inside compile-time execution; see
[signatures.loom](signatures.loom).

`std.meta.record_fields[T]()` returns `Option[List[FieldType]]` with declaration
names and actual compile-time `type` values; `field_type[T](name)` selects one.
Visible refined records expose their base fields without removing construction
checks. Empty records return `Some([])`; non-record and opaque types return
`None`. Library helpers keep their own lexical visibility, so a record used
from another package must be `pub`. Each call returns fresh metadata Lists.
Obtaining a private field's type identity grants no construction or inspection rights.
See [selected_fields.loom](selected_fields.loom); descriptor indices never become
type identities.

`std.meta.enum_variants[T]()` similarly returns `Option[List[VariantType]]`.
Each entry has its declaration name and a `payload` tuple type: no-payload
variants have an empty tuple, one field has a one-element tuple, and multiple
fields retain their declared order. `comptime for/map name, Payload in T`
can iterate those tuples directly. This inspects a visible enum **type**, not
the selected variant of an enum value; no constructor is executed. The same
visibility and checked-construction rules apply. See [variants.loom](variants.loom).

`std.meta.refinement_base[T]()` returns `Option[type]` for a visible refinement's
non-refined base. It returns `None` for non-refined types or inaccessible nominal
definitions; abstract queries remain deferred until selection is known. The
result is only compile-time identity: it grants neither construction permission
nor predicate evidence. See [refinements.loom](refinements.loom).

A type-producing helper may depend on an unknown generic type. Its result stays
opaque during generic checking; `comptime if` selects and checks the concrete
branch at specialization. This neither assumes that the result equals the input
nor adds undeclared capabilities. Known structural results still retain their
symbolic elements. See [dependent.loom](dependent.loom).

`std.meta.tuple(elements)` and `std.meta.function(parameters, output)` build
ordinary tuple/function types from computed `List[type]` inputs. The optional
output is `None` for no result. Loops may select the elements; the result has
the same identity and checks as a directly written structural type. See
[construction.loom](construction.loom) for native annotations and callbacks.

An immutable binding of a computed type can supply local annotations, generic
arguments, visible nominal constructors and exact-type record/enum patterns.
It also supplies type equality and `implements` operands in `comptime if`.
An explicit `comptime receiver type` parameter supports the same guards and
associated projections; unknown selection remains deferred until specialization.
See [projections.loom](projections.loom). Runtime shadows remain values, and enum
variants selected through a type binding remain enum values.
Static field and type-pack iteration uses the same constructors and patterns;
[static_nominal.loom](static_nominal.loom) selects record, enum and refinement
shapes with `comptime if`. An unknown generic shape does not gain undeclared
construction capabilities. `comptime { ... }` can also appear
directly in local type annotations and explicit generic arguments. Checks and
visibility still apply to the selected concrete type. No type registry, erased
conversion or type-producing helper enters native code.

Generic type construction needs an explicit static boundary: `std.meta.list`
takes a `comptime` type argument. A type-valued local during evaluation is not
itself a generic parameter or a new declaration; declaration signatures continue
to use the ordinary `[T]` parameter mechanism. Runtime values cannot select
types or be captured by a `comptime` block.
