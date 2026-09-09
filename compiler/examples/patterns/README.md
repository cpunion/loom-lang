# Nested and literal patterns

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
Int, Bool, Text and Float literals can appear at any pattern position, such as
`Tagged.Value("ready", true, task)`. Matching uses ordinary equality, with no
implicit Int/Float conversion. `true` and `false` exhaust Bool; the other scalar
types require a wildcard or binding fallback. Numerically equal spellings are
one case, including Float `0.0` and `-0.0`; NaN reaches the fallback. The example
also checks Int endpoints, decoded Unicode/NUL text, Float infinities, overlapping
tuple cases, and a proved Boolean-to-integer function.

The implementation uses ordinary typed matches, comparisons and fields, not a
runtime matcher. Input expressions are evaluated once.

This adds enum/tuple/literal patterns in `match`, not record patterns, guards,
or nested `let`/`var` destructuring. Resource aggregate restrictions still apply.
