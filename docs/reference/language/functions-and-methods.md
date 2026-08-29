# Functions and methods

> Normative for Loom language version 0.3.

## Functions

A function declares its parameter types and, when non-Unit, its return type:

```loom
fn combine(left Int, right Int) Int {
    left + right
}
```

The type follows the parameter name without a colon. The return type follows
the closing parenthesis without `->`.

Parameters are immutable. Their expressions are evaluated at the call site
from left to right, then the function body executes. Loom does not overload
functions by parameter type or arity.

Generic functions place type parameters after the name:

```loom
fn first[A, B](pair (A, B)) A {
    let left, right = pair
    left
}
```

Calls normally infer type arguments. Explicit arguments, when needed, precede
the call parentheses: `first[Int, Text](pair)`.

## Return types and body values

Omitting the return type means exactly `Unit`; Loom does not infer a return type
from the body:

```loom
fn announce(message Text) {
    discard message.length()
}
```

For a non-Unit function, every normally completing path must produce the
declared type through its body tail or an explicit `return`. For a Unit function,
an empty body or a body ending after a statement supplies the implicit Unit
tail. A final non-Unit expression in an omitted-return function is a type error,
not an implicit discard.

The callable syntax forbids an explicit bare `Unit` return annotation and a
direct final bare `Unit` expression in the callable body. Both are implicit.
This restriction does not hide the user-visible `Unit` type or value: `Unit`
cannot be omitted inside another type such as `Result[Unit, E]` or `Task[Unit]`,
and expressions such as `Ok(Unit)` remain valid.

## Methods

Methods are declared in an inherent implementation:

```loom
impl Counter {
    method value(self) Int {
        self.current
    }

    method increment(mut self) {
        self.current = self.current + 1
    }
}
```

`self` or `mut self`, when present, is the first item in the parameter list. A
read-only `self` cannot mutate the receiver or call a mutable receiver method.
`mut self` may assign the receiver's fields and call other mutable methods.

A `mut self` call requires a caller `var` place:

```loom
var counter = Counter { current = 0 }
counter.increment()
```

It cannot be invoked on a `let`, temporary, or other read-only place. The call
updates the same caller place; it neither consumes the value nor introduces
source-level reference syntax. Static alias checking rejects overlapping reads
or writes that would conflict with an active mutable receiver call.

Only the target type's owning module may declare inherent methods. Methods are
private by default and may be prefixed with `pub`.

## Concept requirements and conformance methods

A concept requirement uses the same signature form but has no body:

```loom
concept Display {
    method display(self) Text
}
```

A conformance supplies an exact implementation signature. It inherits the
requirement's contracts and cannot redeclare them:

```loom
impl Display for Label {
    method display(self) Text {
        self.text
    }
}
```

Concepts can also require `static method` members, called with a qualified
selection such as `<Int as Zero>.zero()`. Dynamic concepts cannot contain
static methods. See [Generics, concepts, and `dyn`](generics-concepts-and-dyn.md).

## Contracts

Functions, inherent methods, and concept requirements may declare `requires`
and `ensures` clauses between their signature and body:

```loom
fn divide(total Float, count Float) Float
    requires count != 0.0
    ensures result * count == total
{
    total / count
}
```

All `requires` clauses precede all `ensures` clauses. Conformance methods use
the concept requirement's contract. Contract behavior and checking order are
defined in [Constrained types and contracts](constrained-types-and-contracts.md).

## Async functions

An async declaration writes its logical result type:

```loom
record Loaded {
    key Text
}

async fn load(key Text) Loaded {
    Loaded { key = key }
}
```

Calling it produces `Task[Loaded]`; the body result is obtained with the
postfix `.await` inside another async callable. An omitted async return type is
`Unit`, so the call produces `Task[Unit]`. Async methods and async concept
requirements have no declaration syntax in language version 0.3.

See [Async functions and tasks](async-and-tasks.md).

## Tests

Tests are private top-level callables:

```loom
test fn addition_is_exact() {
    assert 20 + 22 == 42
}

test async fn loading_works() Result[Unit, LoadError] {
    load_fixture().await?
    Ok(Unit)
}
```

A test has no parameters, receiver, or generic parameters. It returns `Unit` or
`Result[Unit, E]`. Normal `Unit` or `Ok(Unit)` completion passes. An `Err`,
contract fault, runtime fault, or execution defect fails the test. Tests use the
same type checker, contracts, resource rules, and task rules as other code.
`loomc test` runs tests owned by the selected root package; tests declared by
dependencies are type-checked as dependency source but are not executed.
