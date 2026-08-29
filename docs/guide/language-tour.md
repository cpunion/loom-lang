# Language tour

This tour covers the implemented Core language. Loom is statically typed,
uses ordinary text files and directory packages, and compiles ahead of time by
default. The language is experimental: examples are executable evidence, not a
long-term compatibility guarantee.

## Files, packages, and imports

The directory containing a `.loom` file defines its package. Source files do
not declare a module. Names from another package must be imported explicitly:

```loom
import std.float.is_finite
```

Declarations are private to their package unless marked `pub`. Files in the
same directory share one declaration namespace; a source file's name does not
change that namespace. `module` is an ordinary identifier, not a keyword.

## Types and declarations

Parameters and fields use `name Type`, without a colon. A non-`Unit` return type
follows the parameter list, without an arrow:

```loom
fn choose(flag Bool) Int {
    if flag { 1 } else { 2 }
}
```

Omitting the return type means `Unit`; it does not infer a type from the body.
The return annotation and the callable body's direct bare `Unit` tail are both
omitted:

```loom
fn empty() {}
```

Writing `fn empty() Unit` or ending its body directly with bare `Unit` is a
syntax error. `Unit` remains a user-visible type and value: it must still be
written when it is a type argument, field, or parameter, and may be used in
ordinary expressions such as `Ok(Unit)`. Examples include `Result[Unit, E]`
and `Task[Unit]`.

The built-in scalar types are `Bool`, fixed-width checked signed `Int`, IEEE 754
binary64 `Float`, `Text`, and `Unit`. `Int` is always signed 64-bit; it does not
change width with the target platform. Integer overflow and division errors are
runtime faults. There is no implicit conversion between `Int` and `Float`.

Common generic value types include `(A, B)`, `List[T]`, `TextMap[T]`,
`Option[T]`, and `Result[T, E]`. `Text` contains valid Unicode scalar values;
its indexing and length APIs count Unicode scalars, not UTF-8 bytes or grapheme
clusters.

## Blocks and values

Loom does not use semicolons. A block's final expression is the block value, so
ordinary return paths do not need `return`:

```loom
fn add_one(value Int) Int {
    value + 1
}
```

Use `return` only for an early exit. A bare `return` returns `Unit`.

A non-`Unit` expression in statement position is rejected. Bind it, return it,
pass it onward, or explicitly evaluate and ignore it with `discard`:

```loom
fn observe() {
    discard add_one(41)
}
```

`discard` still evaluates the expression, including its effects, checks, and
faults. It cannot suppress resource or task obligations; those values have to
be cleaned up or joined as described in the dedicated guides.

A tail expression is never implicitly discarded. A callable with an omitted
return type still expects `Unit`, so ending it with `42` is a type error; write
`discard 42` as a statement when ignoring that value is intentional.

## Local state and control flow

`let` creates an immutable binding and `var` creates a mutable binding:

```loom
fn sum_first_four() Int {
    var total = 0
    for value in 0..4 {
        total = total + value
        Unit
    }
    total
}
```

The range is half-open. Its bounds are evaluated once, and each iteration body
is a lexical cleanup scope.

`if` and `match` are expressions. An `if` used for a value requires an `else`,
and conditions are `Bool`; there is no truthiness conversion. Matching a closed
enum must be exhaustive:

```loom
enum LookupError {
    Missing
    Unavailable(Text)
}

fn describe(outcome Result[Text, LookupError]) Text {
    match outcome {
        Ok(value) => value
        Err(LookupError.Missing) => "missing"
        Err(LookupError.Unavailable(reason)) => reason
    }
}
```

There is no fallthrough. `_` is a wildcard when discarding a payload is legal.

## Records, enums, and methods

Record fields also use `name Type`, while record literals assign fields with
`=`:

```loom
record Counter {
    value Int
}

impl Counter {
    method current(self) Int {
        self.value
    }

    method increment(mut self) {
        self.value = self.value + 1
    }
}
```

`self` is read-only by default. A `mut self` method requires a mutable receiver
place, normally a `var` binding, and writes the logical value back on normal
return. Loom exposes no borrow, lifetime, or reference syntax.

Enums are closed nominal types. Variants may have payloads, as in
`LookupError.Unavailable(Text)`. `Option` and `Result` are prelude enums with
`Some`/`None` and `Ok`/`Err` variants.

## Tuples, lists, and maps

Tuples can contain different types and support parallel binding:

```loom
fn pair() (Int, Text) {
    (3, "tuple")
}

fn use_pair() {
    let number, label = pair()
    assert number == 3
    assert label == "tuple"
}
```

Lists are homogeneous and dynamically sized:

```loom
fn numbers() List[Int] {
    var values = List[Int]()
    values.add(10)
    values.add(20)
    values
}
```

`length()` returns `Int`, and `get(index)` returns `Option[T]`; negative and
out-of-range indices produce `None`. `TextMap[T]` is an immutable, canonically
ordered map with `Text` keys. Operations such as `insert` and `remove` return a
new map value.

## Generic functions

Generic parameters use brackets:

```loom
fn value_or[T](value Option[T], fallback T) T {
    match value {
        Some(found) => found
        None => fallback
    }
}
```

Use a concept bound such as `T: Display` when the generic body needs behavior.
Loom uses explicit nominal conformance rather than structural typing. See
[Concepts and polymorphism](concepts-and-polymorphism.md).

## Expected failure and faults

Use `Result[T, E]` for an expected, recoverable outcome and `Option[T]` for
absence. The suffix `?` propagates an `Err` from a callable returning
`Result[_, E]`; the error type must currently match exactly.

Contract failures and runtime failures are different from business errors:

| Channel | Intended use | Catchable as a language value |
| --- | --- | --- |
| `Result[T, E]` | Expected domain, parsing, constraint, or I/O failure | Yes |
| `ContractFault` | Failed `requires`, `ensures`, invariant, or `assert` | No |
| Runtime fault | Overflow, division failure, invalid runtime operation, or OOM | No |

The compiler does not provide a universal `Error` superclass or implicit error
conversion.

## Tests and entry points

An executable entry is an exported, parameterless function returning `Unit`:

```loom
pub fn main() {
    let answer = 2 + 2
    assert answer == 4
}
```

Tests use the same language and runtime, but are declared only in files whose
names end in `_test.loom`:

```loom
test fn arithmetic_works() {
    let answer = 6 * 7
    assert answer == 42
}
```

Asynchronous tests are written `test async fn`. Production commands exclude
`*_test.loom`. `loomc test` adds the selected root module's test files, and
never adds or runs dependency test files.

## Next topics

- [Constraints and contracts](constraints-and-contracts.md)
- [Concepts and polymorphism](concepts-and-polymorphism.md)
- [Resources and cleanup](resources-and-cleanup.md)
- [Asynchronous programming](asynchronous-programming.md)
- [Packages and dependencies](packages-and-dependencies.md)

The current Core deliberately excludes inheritance, ownership and borrowing
syntax, universal dynamic values, reflection-based conformance discovery,
operator overloading, AOP, and source-AST editing.
