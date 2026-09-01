# Types and values

> Normative for Loom language version 0.4.

Loom is statically typed. Every expression has one type before execution, and
ordinary operations do not perform runtime method lookup or implicit type
discovery.

## Built-in value types

The core type vocabulary includes:

| Type | Meaning |
| --- | --- |
| `Bool` | `true` or `false` |
| `Int` | checked signed 64-bit integer |
| `Float` | IEEE 754 binary64 number |
| `Text` | immutable sequence of Unicode scalar values |
| `Bytes` | byte sequence with copy-on-write value semantics |
| `Path` | immutable portable lexical path |
| `Unit` | the single value `Unit` |

The prelude also provides the generic type constructors `Option[T]`,
`Result[T, E]`, `List[T]`, `TextMap[V]`, `Task[T]`, and `TaskOutcome[T]`.
Standard-library operations introduce additional closed value types such as
`Duration`, `Json`, and their error types. See the
[standard library reference](../std/README.md).

`Text` is the language's text type. There are no `String`, `string`, or `str`
aliases, and `Text` does not imply borrowing or a lifetime. Arbitrary binary
data uses `Bytes`; conversion between the two is explicit.

## Compile-time constants

A top-level constant has an explicit primitive type and uses the same
colon-free declaration style as parameters and fields:

```loom
const retry_limit Int = 3
pub const service_name Text = "billing"
const enabled Bool = retry_limit > 0 && service_name == "billing"
```

The declared type must be exactly `Bool`, `Int`, `Float`, or `Text`. An
initializer may contain literals, references to other constants, and the unary
or binary operations ordinarily valid for those primitive types. Binary
operands must have the same type; comparisons produce `Bool`. Declaration and
file order do not affect evaluation. `&&` and `||` short-circuit exactly as
they do at runtime. Cycles, integer overflow, integer division by zero, and
minimum-`Int` division by `-1` are compile-time errors.

Calls, allocation, tuples, lists, records, control flow, I/O, and reads of
runtime state are not constant expressions. A constant is substituted by value
in executable code and contract proofs. It has no runtime storage, stable
address, initialization, cleanup, or destructor.

Constants are private to their directory package by default. Files in the same
package share private constants; `pub const` may be imported from another
package with the ordinary single-symbol `import` syntax.

## Numeric semantics

`Int` has the range -9,223,372,036,854,775,808 through
9,223,372,036,854,775,807 on every target. Addition, subtraction,
multiplication, division, and negation are checked. Overflow, division by zero,
and minimum-`Int` division by `-1` produce an uncatchable `RuntimeFault`.

`Float` follows IEEE 754 binary64 arithmetic. Its ordinary operations may
produce infinities and NaN. Ordered comparisons with NaN are false, equality
with NaN is false, inequality with NaN is true, and `+0.0 == -0.0` is true.

There is no implicit conversion between `Int` and `Float`, and arithmetic
operands must have the same numeric base type. Loom has no numeric cast syntax
or general conversion operator. The explicit `std.float.from_int` and
`std.float.to_int` functions are the only Int/Float conversions. Loom also does
not expose platform-sized integer types.

## Structural and nominal types

Tuple types are structural and may contain different element types:

```loom
fn pair() (Int, Text) {
    (3, "three")
}
```

A one-element tuple includes a trailing comma: `(T,)` and `(value,)`.

`List[T]`, `TextMap[V]`, `Option[T]`, `Result[T, E]`, and `Task[T]` are
structural applications of their type constructors. List elements are
homogeneous; an empty list literal requires an expected `List[T]` type, while
`List[T]()` constructs an explicitly typed empty list.

User-defined records, enums, and constrained types are nominal. Two
declarations remain different types even when their fields, variants, bases,
or predicates have identical spelling.

Every value type must have a finite by-value layout. Record fields, enum
payloads, constrained bases, tuple elements, `Option`, `Result`, and
`TaskOutcome` payloads are inline edges. Nominal type arguments participate in
this rule even when the declaration does not otherwise expose that parameter.
A direct or mutual cycle through only inline edges is rejected as
`RecursiveValueType`; Loom never inserts a hidden box to make it compile.

`List`, `TextMap`, `Task`, and `dyn C` storage are explicit
indirection boundaries and therefore break a layout cycle. For example, a
record may contain `List[Node]` when `Node` is that record, but it cannot contain
`Node` or `Option[Node]` directly. This keeps allocation and failure visible in
the selected type constructors instead of changing a nominal declaration's
ABI implicitly.

## `Option`, `Result`, and absence

Loom has no `null` value. Optional data uses:

```text
Option[T] = None | Some(T)
```

Expected, recoverable failure uses:

```text
Result[T, E] = Ok(T) | Err(E)
```

`None` and `Unit` are values, not zero-argument calls. `Some`, `Ok`, and `Err`
carry one value. `Ok` and `Err` require an expected `Result` type whenever the
other type argument cannot be inferred.

An `Err` is ordinary data. It does not unwind the stack unless the postfix `?`
operator explicitly propagates it. Contract and runtime faults are separate
failure channels and are not automatically converted to `Err`.

## `Unit` and omitted returns

`Unit` has exactly one value, also spelled `Unit`. A callable with no declared
return type has return type `Unit`; the return type is never inferred from its
body.

```loom
fn notify(message Text) {
    discard message.length()
}
```

A block with no tail expression evaluates to `Unit`. Therefore a Unit-returning
function may end after its final statement or with an empty body. The callable
syntax requires both its bare `Unit` return annotation and a direct final bare
`Unit` expression to be omitted. `Unit` remains a user-visible type and value;
it is explicit in type arguments such as `Result[Unit, E]` and `Task[Unit]`, and
may be used in ordinary expressions such as `Ok(Unit)`.

## Type compatibility and conversions

Loom does not use general implicit coercion. Values must normally have the
expected type exactly. The defined implicit conversions are narrow:

- a constrained value may widen to its declared base type without a check;
- a value may adapt to an interface parameter or `dyn C` when an explicit
  conformance is available and no resource or task obligation would be hidden;
- an expression that never returns is compatible with the expected type of its
  unreachable continuation.

The reverse constrained conversion is never implicit. A base value must call
the constrained type's constructor, which may be proven, rejected, or return a
`Result`. Different constrained types do not implicitly convert to each other,
even when they share a base. There are no implicit numeric, text/byte, enum,
record, or error conversions.

## Copying, mutation, and identity

Ordinary Loom values have value semantics. Assigning or passing a value creates
another logical value; later mutation of one record or dynamic-interface copy
does not mutate another copy. An implementation may share immutable storage,
but storage identity is not observable.

`let` creates an immutable binding and `var` creates a reassignable binding.
Record fields are externally read-only. They may be assigned only through the
owning type's `mut self` method. A mutable receiver operates on the caller's
`var` place and does not require ownership or borrow syntax.

`scoped` resource bindings and values containing a live `Task` have additional
static obligations and cannot be copied or discarded like ordinary values. See
[Memory and resources](memory-and-resources.md) and
[Async functions and tasks](async-and-tasks.md).

## Value equality

`==` and `!=` compare logical values. Equality is available for the primitive
value types above, closed standard value types, and structural or nominal data
whose complete contents also support equality. This includes tuples, lists,
maps, options, results, records, enums, and constrained values when all nested
types support equality.

Equality is not available for files, sockets, tasks, dynamic interface values,
or an unconstrained type parameter. `IoError` is an ordinary record and is
comparable because both of its fields are comparable. There is no
reference-identity operator.
