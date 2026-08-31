# Expressions and control flow

> Normative for Loom language version 0.4.

Loom evaluates expressions and call arguments from left to right. Each source
expression is evaluated once unless control flow does not reach it.

## Primary and postfix expressions

Primary expressions include literals, names, tuples, lists, record literals,
blocks, `if`, and `match`:

```loom
42
"loom"
(1, "one")
[1, 2, 3]
Point { x = 1.0, y = 2.0 }
```

Postfix operations bind more tightly than unary or binary operators:

```loom
make().field
format[Int](value)
load().await
read().await?
```

Function and method arguments are evaluated from left to right. Record-literal
field initializers are evaluated in their source order, regardless of the
record declaration's field order.

A record literal used directly where `if` expects its condition or `match`
expects its scrutinee must be parenthesized so its opening brace is not confused
with the following control-flow block.

## Operators

From highest to lowest precedence, the binary operators are:

| Precedence | Operators | Associativity |
| --- | --- | --- |
| multiplicative | `*`, `/` | left |
| additive | `+`, `-` | left |
| ordered comparison | `<`, `<=`, `>`, `>=` | none |
| equality | `==`, `!=` | none |
| logical and | `&&` | left, short-circuiting |
| logical or | `\|\|` | left, short-circuiting |

Prefix `-` negates an `Int` or `Float`. Prefix `!` negates a `Bool`. Arithmetic
requires two values with the same numeric base type. Ordered comparison accepts
matching `Int` or matching `Float` operands. Equality is described in
[Types and values](types-and-values.md#value-equality).

Comparison operators are non-associative. A chain such as `a < b < c` is a
`ChainedComparison` error; write `a < b && b < c` instead. Parentheses may
explicitly nest comparison results when their types permit it.

`&&` evaluates its right operand only when the left operand is true. `||`
evaluates its right operand only when the left operand is false.

## Blocks and tail values

A block is an expression. Its items are separated by newlines, and its final
bare expression is the block's value:

```loom
let total = {
    let subtotal = 40
    let tax = 2
    subtotal + tax
}
```

If the last item is a local binding, assignment, loop, `defer`, `discard`, or
`assert`, or if the block is empty, the block's value is `Unit`. A non-final bare
expression must have type `Unit`; otherwise it is an `UnusedValue` error and
must be used or explicitly discarded.

Loom has no semicolon syntax. A comma separates elements and selected braced
items; it does not turn an expression into a statement.

## Local bindings and assignment

Local bindings infer their type from the initializer:

```loom
let immutable = 1
var mutable = 2
mutable = mutable + 1
```

Ordinary `let` and `var` bindings do not accept source type annotations. In
particular, `let value: Int = 1` is invalid. Parameters, fields, and return types
carry explicit types; local inference avoids a second declaration style.

An immutable tuple can be destructured by listing multiple names:

```loom
let number, label = (7, "seven")
```

The value must be a tuple with the same arity. Multiple-name `var` and `scoped`
bindings are not permitted.

Only a writable place can appear on the left of `=`. A `var` binding itself is
writable. A record field can be assigned only through `self` inside a
`mut self` method of the owning type; ordinary code cannot use field assignment
to bypass the type's methods or invariant.

`scoped` is a specialized resource binding with an optional annotation and is
covered in [Memory and resources](memory-and-resources.md).

## `if`

`if` is an expression:

```loom
let magnitude = if value >= 0 {
    value
} else {
    -value
}
```

The condition has type `Bool`. When a value is required, an `else` branch is
required and both branches must have compatible types. An `if` without `else`
is permitted in a Unit statement context; its then block must also have type
`Unit`. `else if` is accepted.

Each branch block is its own lexical cleanup scope.

## `match`

`match` evaluates its scrutinee once, selects the first matching arm, and
evaluates that arm's expression:

```loom
let value = match outcome {
    Ok(number) => number
    Err(_) => 0
}
```

Arm expressions must have compatible types. Matches over closed finite types
such as `Bool`, `Option`, `Result`, user enums, and standard closed enums must be
exhaustive. An arm made unreachable by earlier arms is rejected. There is no
fallthrough and no pattern-guard syntax.

Patterns are defined in
[Records, enums, and patterns](records-enums-and-patterns.md#patterns-and-exhaustiveness).

## Half-open range loops

The loop form is:

```loom
for index in start..end {
    use_index(index)
}
```

Both bounds have type `Int` and are evaluated once, before iteration. The loop
visits the half-open range from `start` up to but excluding `end`. It performs no
iterations when `start >= end`. The iteration binding is immutable and scoped
to the loop body. A `for` loop has type `Unit`.

## Conditional loops and loop control

`while condition { ... }` evaluates its `Bool` condition before every
iteration and has type `Unit`. A false initial condition performs no
iterations.

Bare `break` exits the nearest enclosing `while` or `for` loop. Bare `continue`
starts its next iteration: it re-evaluates a `while` condition, and advances a
`for` induction binding before testing the range again. Neither form accepts a
label or value, and either form is an error outside a loop.

Both transfers exit intervening lexical scopes normally. Registered `defer`
blocks and `scoped` disposals therefore run in LIFO order before control reaches
the loop target. A deferred cleanup cannot use `break` or `continue` to control
a loop outside that cleanup, although a loop wholly inside the cleanup may use
its own loop control.

## Return and propagation

`return expression` returns early from the current callable. Bare `return` is
valid only when that callable returns `Unit`.

The postfix operator `?` applies to `Result[T, E]`:

- `Ok(value)?` evaluates to the contained `T`;
- `Err(error)?` returns `Err(error)` from the current callable.

The current callable must return `Result[_, E]` with exactly the same error
type. Loom performs no implicit error conversion. A propagated error exits
lexical scopes normally, so their registered resource and `defer` cleanups run.
`?` is not permitted inside a `defer` cleanup.

## Explicit discard

An ordinary non-Unit expression may be evaluated solely for its observable
effects by writing:

```loom
discard calculate_preview()
```

`discard` always evaluates its operand and then ignores the final value. It is
not a property of a function or type. Values carrying a `MustScope` resource or
a live `Task` cannot be discarded, directly or through an aggregate. A value
whose generic shape cannot prove the absence of either obligation is also
rejected. See [Memory and resources](memory-and-resources.md#discard-and-static-obligations).

## Assertions and deferred cleanup

`assert condition` requires a `Bool` condition. Failure produces an
`AssertionFault`, not an `Err` value. Assertions use the restricted contract
expression subset described in
[Constrained types and contracts](constrained-types-and-contracts.md#contract-expressions).

`defer { ... }` registers block-level cleanup and is described in
[Memory and resources](memory-and-resources.md#defer).
