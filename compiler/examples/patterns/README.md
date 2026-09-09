# Nested patterns

```sh
target/loom check compiler/examples/patterns
target/loom test compiler/examples/patterns
target/loom run compiler/examples/patterns
```

Match enum payloads and tuples directly, for example
`Result.Ok(Option.Some((label, values)))` or `(Option.Some(first), second)`.
Bare payload names bind immutable values; `_` ignores a value only when its
type allows that. Use qualified names for payload variants without fields, such
as `Option.None`. Tuple patterns require a comma, including `(value,)`.

The first matching arm wins. Every possible combination must be covered, and an
arm covered entirely by earlier arms is rejected. The compiler checks these rules
from the types, not just the values currently passed by callers.

The example exercises shared Lists, a once-evaluated input, compile-time matching,
and Task payloads across real timer waits. A whole fallback retains the value even
after a nested discriminant was examined; it cannot duplicate or drop its Tasks.
The implementation uses ordinary typed matches and fields, not a runtime matcher.

This adds enum/tuple patterns in `match`, not literal/record patterns, guards,
or nested `let`/`var` destructuring. Resource aggregate restrictions still apply.
