# Predicate reflection

`describe[T]()` returns `Option[Predicate]` for a refinement visible at the call
site. The descriptor contains a display-only owner name, canonical expression
source, and its public `std.loom.ast.Node`. Spans refer to that returned source.

```loom
import std.reflect.predicates.describe
import std.option.Option

type Positive = Int where self > 0

fn main() {
    assert match describe[Positive]() {
        Option.Some(predicate) => predicate.expression.value == ">"
        Option.None => false
    }
}
```

Only the direct predicate is described. Ordinary and inaccessible types return
`None`; a generic wrapper retains its own lexical visibility. Use
`std.reflect.describe` separately for the base type. Names in predicate syntax
retain their declaration-site spelling. Helper bodies are not expanded, and
descriptors grant neither access to private helpers nor proof evidence.

Queries work at compile time and in native programs. Each evaluation creates
fresh AST Lists; copies share those Lists without changing the type definition.
There is no runtime reflection registry, and describing a predicate does not
retain its helpers as native roots. See the [executable example](../../../examples/reflection/predicates.loom).
