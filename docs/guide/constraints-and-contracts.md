# Constraints and contracts

Loom separates values that have been established to satisfy a predicate from
ordinary base values, and separates expected construction failure from program
contract faults.

The executable companion for this guide is
[`examples/core01/shop.loom`](../../examples/core01/shop.loom).

## Constrained nominal values

Define a constrained type with a base type and a `where` predicate:

```loom
import standard.float.is_finite

type Money = Float where is_finite(self) && self >= 0.0
```

`Money` is a nominal type, not an alias. Construct it explicitly with
`Money(value)`. The checker assigns one of three outcomes:

| What the checker can prove | Source result | Runtime check |
| --- | --- | --- |
| Predicate is true | `Money` | Eliminated |
| Predicate is false | Compile-time `ConstraintUnsatisfied` diagnostic | No program is produced |
| Predicate is unknown | `Result[Money, ConstraintError]` | Performed by both backends |

For example:

```loom
fn literal() Money {
    Money(10.0)
}

fn checked(raw Float) Result[Money, ConstraintError] {
    Money(raw)
}
```

`Money(-1.0)` is rejected statically. A value arriving through `raw` is checked
at runtime because the compiler cannot assume the caller's data.

The proof engine also uses established branch conditions, prior successful
assertions, contracts, and nominal facts. It is conservative across mutation,
loops, impure calls, and paths it cannot model. An unknown fact keeps the check;
it is never treated as true for optimization.

## Conversions are one-way

A constrained numeric value can be used where its base type is expected:

```loom
fn widen(value Money) Float {
    value
}
```

The reverse direction always names the constrained constructor. It is either
proved, rejected, or returns `Result` according to the table above. Loom does
not implicitly narrow `Float` to `Money`.

Two constrained types with the same base and even the same predicate remain
different nominal types. There is no implicit conversion between them. Use the
destination constructor explicitly so its predicate is checked or proved:

```loom
type Positive = Float where is_finite(self) && self > 0.0

fn positive_to_money(value Positive) Money {
    Money(value)
}
```

Arithmetic on a constrained number produces the base numeric type. Reconstruct
the constrained type when the result must carry that invariant:

```loom
fn add_money(left Money, right Money) Result[Money, ConstraintError] {
    Money(left + right)
}
```

There is no implicit `Int`/`Float` conversion and no general coercion hierarchy.

## Record invariants

A record may declare an invariant over its fields:

```loom
record Range {
    low Money
    high Money

    invariant self.low <= self.high
}
```

Record construction uses the same three outcomes as constrained construction:
a proven literal has type `Range`, a statically false literal is an
`InvariantUnsatisfied` error, and an unknown invariant produces
`Result[Range, ConstraintError]`.

An invariant is checked at method entry and normal method exit. A `mut self`
method may temporarily invalidate it while executing, but cannot let the
partially updated `self` escape or call another method through that invalid
state. Normal exits, including a returned `Err`, re-establish the invariant.

## Preconditions, postconditions, and assertions

Attach contracts to a callable between its signature and body:

```loom
impl Range {
    method width(self) Float
    requires self.low <= self.high
    ensures result >= 0.0
    {
        self.high - self.low
    }
}
```

- `requires` checks a caller obligation at entry.
- `ensures` checks the returned logical value through `result`.
- `invariant` checks nominal record state at construction and method boundaries.
- `assert` checks a fact at its source position.

Contracts are enabled in every build profile. Release mode does not globally
disable them.

If the compiler independently proves a contract clause from established facts,
it omits that runtime check. It does not assume a clause merely because the
clause itself says the condition is true. Unknown or disproven clauses remain
in checked MIR so both interpreter and LLVM executions observe the same fault.

## Entry snapshots with `old`

`old(expression)` is available only in `ensures`. It captures a logical deep
value snapshot after preconditions pass and before the body begins:

```loom
method apply_discount(mut self, discount Price) Result[Unit, DiscountError]
ensures match result {
    Ok(_) => self.discount == discount
    Err(_) => self.discount == old(self.discount)
}
{
    if discount > self.subtotal {
        return Err(DiscountError.ExceedsSubtotal)
    }
    self.discount = discount
    Ok(Unit)
}
```

Later mutation cannot alter the snapshot. `old` is not an address, reference,
or lazy callback. Erased interface values and other unsupported shapes cannot
be snapshotted.

## The contract expression subset

Contract expressions are intentionally closed and total. They include literals,
parameters, fields, `self`, `result`, valid `old` expressions, Boolean logic,
comparisons, Float arithmetic, exhaustive matching, and the compiler-known
pure predicate `standard.float.is_finite`.

User function calls, indexing, assignment, I/O, checked construction, and other
potentially effectful operations are rejected in a contract. Checked `Int`
arithmetic is also excluded because it can fault. `Int` literals and comparisons
remain valid.

This restriction keeps contracts executable without requiring a general effect
or termination system.

## Failure model

`ConstraintError` is a concrete prelude failure value used only when a
constrained value or record invariant cannot be established dynamically. It is
not a universal error superclass.

A failed `requires`, `ensures`, invariant boundary, or `assert` produces an
uncatchable `ContractFault` with an appropriate diagnostic category. It is not
converted to `Err`, and witness or dynamic dispatch does not bypass it.

Integer overflow, division errors, OOM, and runtime ABI failures are runtime
faults rather than contract failures. Expected parsing, I/O, and business
failure should continue to use an explicit `Result` error enum.
