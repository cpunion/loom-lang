# Nested, literal, record and guarded patterns

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

Records use named fields: `Packet { value = ("ready", items), ready = true }`.
Fields may be reordered and are separated by commas or newlines. List every
field or explicitly omit the rest with `..`, as in `Packet { value = item, .. }`.
Omitted fields follow ordinary discard rules; this cannot erase Tasks or scoped
resources. Names identify the actual nominal record; generic arguments come from
the matched value. Record patterns nest inside enum/tuple/record patterns, and a
whole fallback preserves every remaining field and shared alias. See `records.loom`.

The implementation uses ordinary typed matches, comparisons and fields, not a
runtime matcher. Input expressions are evaluated once.

Record/tuple bindings also work in `let` and `var`, for example
`let Packet { value = (text, items), .. } = packet`. The initializer runs once,
before any bound name enters scope. `var` makes each name rebindable; it does not
turn those names into references to the record's fields. Shared Lists still
share their contents. Omitted fields cannot drop Tasks or MustScope resources,
and a MustScope record still requires `scoped`, not destructuring.

`pattern if condition => body` tests its Boolean guard only after the pattern
matches, with bindings in scope. A false guard keeps its effects and tries the
next arm. Guards do not establish exhaustive coverage. `guards.loom` exercises
source order, shared mutation, guard cleanup/returns, compile-time evaluation,
timer waits and Task payload retention. Consuming a Task needed by a later path
rejects; guarded matches containing MustScope resources currently reject.

`multiline.loom` also uses raw triple-quoted Text for native and compile-time
comparisons and patterns. Closing indentation is removed without interpreting
backslash escapes; longer quote delimiters can embed triple quotes.

Literal and enum patterns remain `match`-only. Field shorthand, scoped
destructuring and parallel reassignment are not implemented.
