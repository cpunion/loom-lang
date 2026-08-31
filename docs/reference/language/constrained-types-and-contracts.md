# Constrained types and contracts

> Normative for Loom language version 0.4.

Loom distinguishes data validation from implementation correctness. A
constrained constructor may return ordinary `Result` data; a violated function
contract produces a non-recoverable `ContractFault`.

## Nominal constrained types

A constrained type gives a base value a new nominal identity and a predicate:

```loom
import std.float.is_finite

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
not a `ContractFault`. Its fields are `target_type Text`, `code Text`,
`predicate Text`, `path List[Text]`, `value_summary Text`, and
`contract_span (Int, Int, Int)`. The value summary identifies only the rejected
value's type; it never includes the value, contents, size, variant, or nested
data.

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

Every method call on an established record checks its receiver invariant at
entry. A `mut self` method may temporarily make the receiver invalid, but while
it is invalid the receiver is isolated: it cannot be copied, passed, returned,
stored, or used for another method call. A successful `assert` that
re-establishes the invariant ends that isolation. Every normal mutable-method
exit, including an `Err` result, must restore and recheck the invariant. A
read-only method cannot mutate the established receiver and may transfer it, so
it has no exit invariant replay.

Mutation cannot bypass that boundary. Calling a mutable method on a projected
field below an invariant-bearing record is `InvariantInteriorMutation`; call a
`mut self` method on the complete invariant-bearing record instead. The only
protected prefix a projected mutable call may cross is the current method's own
`self` root. Such a call makes `self` isolated until its invariant is proven
again, and a second invariant-bearing record nested below `self` remains a
separate boundary. Read-only projection and operations on the complete value do
not violate this rule.

The rule is independent of dispatch. Mutable adaptation to `dyn C` and mutable
interface reborrowing cannot hide the owner place: the compiler applies the
same complete-value, current-`self`, and nested-boundary checks to that owner.
On a fault edge, an inout writeback may contain the callee's partial mutation.
Such an invariant-bearing place is unavailable to `defer` and `scoped`
cleanup until cleanup replaces the complete place with an established value.
This prevents fault handling from observing a value for which the invariant is
not known.

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

A synchronous call checks `requires` after evaluating its arguments and before
entering the body. An async call first creates its child `Task`; that child
checks `requires` in state zero before entering the async body. Failure keeps the
creation expression as its blame site but becomes the child's primary fault, so
creating the Task does not unwind the parent. An exported async root has no
creation expression and uses its declaration span for blame.

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

An `ensures` clause cannot inspect `self`, an argument, or an `old` snapshot
whose type contains a `Task`; the body may already have transferred that affine
input. The same structural inspection is valid in `requires`, and `ensures`
may inspect a Task-bearing `result`. These reads are non-consuming compiler
operations and introduce no source ownership or borrow syntax.

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
- checked Int arithmetic and Float arithmetic;
- exhaustive `match` expressions whose contents remain in the subset;
- the imported total predicate `std.float.is_finite`.

Int `+`, `-`, `*`, `/`, and unary negation remain checked as they are in
ordinary code. Overflow and division by zero therefore produce their original
`RuntimeFault`; they are not reported as contract violations. Only a completed
predicate whose value is `false` produces the clause's contract fault. User
function calls, method calls, mutation, I/O, record or collection construction,
blocks, `if`, `.await`, `?`, `return`, and task operations are not contract
expressions. A contract may read an immutable local in scope at an `assert`,
but not a mutable `var`.

## Checking order

For a synchronous closed-world function call, the observable order is:

```text
evaluate arguments -> requires -> old snapshots -> body -> lexical cleanup -> ensures
```

For a synchronous inherent or concept method, the entry invariant precedes the
body and only a mutable receiver is rechecked at exit:

```text
evaluate receiver and arguments -> requires -> entry invariant -> old snapshots
-> body -> lexical cleanup -> exit invariant (mut self only) -> ensures
```

An async call has two observable phases:

```text
caller: evaluate receiver and arguments -> create child Task
child state zero: entry invariant (method only) -> requires -> old snapshots
-> body -> lexical cleanup -> exit invariant (mut self only) -> ensures
```

The child retains the creation expression as precondition blame. An exported
async root substitutes its declaration span because no caller expression
exists. A state-zero failure faults the child and does not unwind the creator.

If the body returns normally through either a tail expression or `return`, the
lexical cleanup suffix runs before exit checks. An `Err` is a normal return and
therefore also follows this sequence. A cleanup fault remains primary and no
exit contract runs. For a mutable receiver, when both the exit invariant and
postcondition would fail, the earlier invariant check determines the reported
fault.

Conformance calls use the concept requirement's contract and the concrete
receiver's invariant. Static dispatch, interface parameters, and first-class
`dyn` dispatch follow the synchronous or async order appropriate to the
requirement.
