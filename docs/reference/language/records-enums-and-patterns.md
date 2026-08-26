# Records, enums, and patterns

> Normative for Loom language version 0.3.

## Records

A record is a closed nominal product with named fields:

```loom
record Pair[A, B] {
    first A
    second B
}
```

Field declarations use `name Type`. Every field is required when constructing
a record, and each field may appear exactly once:

```loom
let pair = Pair {
    first = 7
    second = "seven"
}
```

Field initializer expressions run from left to right in literal source order.
The declaration order does not create tuple indices, affect equality, or grant
an implicit external representation.

Fields are readable through dot selection. They are externally read-only and
can be assigned only through `self` inside a `mut self` method owned by the
record type. Records have value semantics: mutating one logical copy does not
mutate another.

A record may declare one invariant after its fields:

```loom
record Range {
    low Float
    high Float

    invariant self.low <= self.high
}
```

Invariant-bearing construction can produce the record directly, be rejected
statically, or produce `Result[Range, ConstraintError]`, depending on what the
language proof rules establish. See
[Constrained types and contracts](constrained-types-and-contracts.md#record-invariants).

## Enums

An enum is a closed nominal sum. A variant may have no payload or any fixed
number of typed payload values:

```loom
enum ParseOutcome[T] {
    Empty
    Value(T)
    Partial(T, Int)
}
```

User variant construction is qualified by the enum name:

```loom
fn empty_int() ParseOutcome[Int] {
    ParseOutcome.Empty
}

let value = ParseOutcome.Value(42)
let partial = ParseOutcome.Partial(42, 3)
```

A zero-payload variant of a generic enum needs an expected enum type because
its expression contains no payload from which to infer the type arguments.

Payload expressions are evaluated left to right. Variant order has no source
level integer value or priority. Loom has no open enums, implicit discriminants,
or enum inheritance.

The prelude's `Some`, `None`, `Ok`, and `Err` are the built-in short constructor
names for `Option` and `Result`.

## Match expressions

Enums are consumed with `match`:

```loom
fn value_or_zero(value ParseOutcome[Int]) Int {
    match value {
        Empty => 0
        Value(found) => found
        Partial(found, _) => found
    }
}
```

The expected scrutinee type resolves short variant names in patterns, so
`Value(found)` is sufficient. A qualified pattern such as
`ParseOutcome.Value(found)` is also accepted.

## Patterns and exhaustiveness

Language version 0.3 supports:

- `_` wildcard patterns;
- `Bool`, `Int`, `Float`, and `Text` literal patterns;
- immutable name bindings;
- zero-payload variant patterns;
- recursively nested variant patterns with payloads.

```loom
match result {
    Ok(Some(value)) => value
    Ok(None) => fallback
    Err(ParseError.Invalid) => fallback
}
```

A bare name is resolved as a variant when it names a variant of the expected
scrutinee type; otherwise it introduces an immutable binding. A negative number
is a unary expression and therefore cannot be used as a literal pattern.

Record, tuple, list, range, or-pattern, and guarded patterns have no syntax in
language version 0.3.

Matches over closed finite types are checked for exhaustiveness and usefulness.
This includes `Bool`, `Unit`, `Option`, `Result`, user enums, `TaskOutcome`, and
the closed standard-library enums. The checker examines nested finite payloads,
so matching a variant without covering all of a nested `Bool` or enum payload
does not make the match exhaustive. A wildcard or binding covers every value of
its expected type. Duplicate or shadowed arms are rejected as
`UnreachableMatchArm`.

Every arm introduces its own binding scope. A resource or task obligation in a
payload cannot be silently discarded with `_`; the binding must be handled by
the rules in [Memory and resources](memory-and-resources.md) or
[Async functions and tasks](async-and-tasks.md).

## Derived equality

A record supports `==` and `!=` when all of its fields support value equality.
An enum supports equality when every payload type of every variant supports
equality. The comparison is semantic: record declaration order and enum
implementation details are not observable.
