# Predicate reflection and weakening

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

`implies[From, To]()` asks the compiler whether the ordinary checked-construction
rules prove a refinement weakening over the same base type. It uses type
guarantees, not facts about a particular caller value. Private predicate helpers
resolve in their defining packages; querying them does not retain native code.

```loom
import std.reflect.predicates.implies

type Positive = Int where self > 0
type Nonnegative = Int where self >= 0

fn main() {
    assert implies[Positive, Nonnegative]()
    assert !implies[Nonnegative, Positive]()
}
```

True is a checked weakening proof. False means **unproved**, including unsupported
reasoning; it is not evidence of incompatibility. Numeric conversions, dyn
boxing, database schemas and application writers are not covered by this query.
Use it in `comptime if` to select a checked path or require an explicit migration.
Ordinary generic `requires` clauses may also use the query or a pure wrapper.
Abstract proofs treat unresolved queries as independent Boolean choices, not
implication axioms; concrete instances still compute and check the real result.

`weakening[From, To]()` returns `Option[Weakening[From, To]]` carrying that proof.
Its private state binds both endpoint types: fields alone cannot mint an
unproved witness or rebind one to another pair. This is ordinary source data, not a
runtime proof registry or an unchecked conversion operation. See the
[weakening example](../../../examples/reflection/weakening.loom).
