# Constrained types and contracts

> Normative for Loom language version 0.3.

Loom distinguishes data validation from implementation correctness. A
constrained constructor may return ordinary `Result` data; a violated function
contract produces a non-recoverable `ContractFault`.

## Nominal constrained types

A constrained type gives a base value a new nominal identity and a predicate:

```loom
import standard.float.is_finite

type Money = Float where is_finite(self) && self >= 0.0
```

`Money` is not an alias for `Float`. Two constrained declarations with the same
base and predicate are still different types.

Construction calls the type with one base value:

```loom
Money(expression)
```

The initializer expression is evaluated once. The language then classifies the
predicate against its fixed proof rules:

| Classification | Static type and behavior |
| --- | --- |
| proven true | `Money`; no runtime predicate check |
| proven false | static `ConstraintUnsatisfied` error |
| unknown | `Result[Money, ConstraintError]`; the predicate runs at runtime |

Consequently, `Money(10.0)` has type `Money`, while construction from an
unconstrained input usually has a Result type:

```loom
fn checked(raw Float) Result[Money, ConstraintError] {
    Money(raw)
}
```

At runtime, an unknown construction produces `Ok(value)` when the predicate is
true and `Err(ConstraintError)` when it is false. `ConstraintError` is a
specific, structured validation value, not a universal `Error` supertype and
not a `ContractFault`.

## Conversion rules

An established constrained value widens implicitly to its base without another
check:

```loom
fn amount(value Money) Float {
    value
}
```

The base does not narrow implicitly to the constrained type. It must pass
through the constructor. Different constrained types do not implicitly convert
to each other, even when they share a base.

Arithmetic on constrained numeric values uses their base numeric operation and
produces the base type. The result must be reconstructed when a constrained
result is required. Constructing from an already established value may be
proved check-free when its facts imply the target predicate.

## The proof boundary

The proof classifier is conservative and deterministic. It uses facts visible
in source semantics, including:

- literals and folded constant expressions;
- predicates guaranteed by already established constrained values and records;
- immutable local and tuple-binding propagation;
- successful `requires` and `assert` predicates;
- proof-pure `if` branch conditions;
- simple ordered bounds over the same numeric term.

Mutation invalidates overlapping facts. At a control-flow join, only facts true
on every reachable incoming path remain. A loop includes its zero-iteration
path. General user-function bodies, arbitrary algebraic identities, loop
invariants, and match relationships are not used as hidden cross-call proofs.

For floating-point facts, a false comparison does not imply its mathematical
opposite because NaN makes ordered comparisons false. Finiteness must be
established separately when a predicate requires it.

Only a proof of truth removes a runtime contract check. Proof uncertainty does
not make a program unsound: it keeps the explicit Result or runtime check.

## Record invariants

A record may have one invariant:

```loom
record Range {
    low Money
    high Money

    invariant self.low <= self.high
}
```

All field initializers run before the invariant is considered. Construction
uses the same three-way classification as a constrained type:

- proven invariant: the literal has type `Range`;
- disproven invariant: static `InvariantUnsatisfied` error;
- unknown invariant: `Result[Range, ConstraintError]` and a runtime check.

Every method call on an established record checks its receiver invariant at the
method boundary. A `mut self` method may temporarily make the receiver invalid,
but while it is invalid the receiver is isolated: it cannot be copied, passed,
returned, stored, or used for another method call. A successful `assert` that
re-establishes the invariant ends that isolation. Every normal method exit,
including an `Err` result, must restore the invariant.

Failure while establishing a new value is `ConstraintError`. Failure because a
method received or left an already established value with a broken invariant is
an `InvariantFault`.

## Function and method contracts

`requires` states a caller obligation and `ensures` states a normal-return
promise:

```loom
fn clamp(value Float, low Float, high Float) Float
    requires low <= high
    ensures result >= low && result <= high
{
    if value < low {
        low
    } else if value > high {
        high
    } else {
        value
    }
}
```

Multiple clauses are combined logically. Every `requires` clause must precede
every `ensures` clause.

`result` is available only in an `ensures` expression and denotes the complete
logical return value. `old(expression)` is also available only in `ensures` and
denotes a value snapshot taken at callable entry. Its expression may use entry
parameters and, for a method, `self` and its fields. The snapshot has value
semantics; later mutation does not change it.

An interface parameter cannot be the operand of `old`; attempting to snapshot
one is an `OldOfView` source error. Both concise `value C` and explicit
`value dyn C` parameter spellings follow this rule.

`assert predicate` states an implementation obligation at a point in a body.
It may also establish facts for the code that follows.

Contract failures are classified as follows:

| Form | Failure code | Blame |
| --- | --- | --- |
| `requires` | `PreconditionFault` | caller |
| `ensures` | `PostconditionFault` | implementation |
| method invariant boundary | `InvariantFault` | implementation boundary |
| `assert` | `AssertionFault` | assertion site |

These failures terminate ordinary control flow. They are not exceptions that
source code can catch, are not undefined behavior, and are not converted to
`Result`. Expected business rejection belongs in an explicit Result type.

## Contract expressions

Contract predicates have type `Bool` and use a restricted expression subset.
The subset accepts:

- literals, immutable parameters and locals, `self`, fields, and `result`;
- `old(...)` in `ensures`;
- unary numeric and Boolean operators;
- Boolean operators, equality, and numeric comparisons;
- Float arithmetic;
- exhaustive `match` expressions whose contents remain in the subset;
- the imported total predicate `standard.float.is_finite`.

Binary Int arithmetic is excluded because checked overflow and division by zero
would make it non-total. User function calls, method calls, mutation, I/O,
record or collection construction, blocks, `if`, `.await`, `?`, `return`, and
task operations are not contract expressions. A contract may read an immutable
local in scope at an `assert`, but not a mutable `var`.

Unary Int negation remains checked as it is in ordinary code. Negating the
minimum Int therefore produces a `RuntimeFault`; it is not reported as a
contract violation.

## Checking order

For a function, the observable order is:

```text
requires -> old snapshots -> body -> ensures
```

For an inherent or concept method, receiver invariants surround the same
sequence:

```text
entry invariant -> requires -> old snapshots -> body -> exit invariant -> ensures
```

If the body returns normally through either a tail expression or `return`, the
exit checks run. An `Err` is a normal return and therefore also runs them. When
both the exit invariant and postcondition would fail, the earlier invariant
check determines the reported fault.

Conformance calls use the concept requirement's contract and the concrete
receiver's invariant. Static dispatch, interface parameters, and first-class
`dyn` dispatch have the same contract order.
