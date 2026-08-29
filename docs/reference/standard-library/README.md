# Standard library reference

> Normative for Loom language version 0.3.

The standard library is deliberately small. Its value types follow the same
static typing, value semantics, contracts, resource obligations, and task rules
as user declarations. Standard functions are available only after an explicit
import; standard value types and their built-in constructors and methods are in
the prelude.

The compiler-owned source package is `std`. Source imports library APIs by
their `std.*` path.

Source-backed modules are distributed as Loom source and compile through the
ordinary module, type, MIR, reachability, and native pipelines. The current
source package contains the foundational `std.int` algorithms, the
`std.log.debug`, `info`, `warn`, and `error` convenience functions, and the
public `Dispose`, `MustScope`, and `NoSuspend` declarations in `std.resource`.
The logging conveniences are ordinary Loom functions over the irreducible
`std.log.write` output boundary. Resource declarations are source-backed, but
their fixed shapes and irreducible static rules remain part of the language
core and add no runtime registry. Other documented APIs, including JSON,
currently use compiler-known or runtime implementations until their Loom
source modules exist. Those private paths are deleted after their source
replacements pass the ordinary
pipeline gates; they are not retained as compatibility layers. The target
boundary permits only irreducible GC, scheduler, platform, and generic
construction services to cross into the compiler-private runtime. The
implementation rule and source-replacement gates are documented in
[Core, standard library, and runtime boundary](../../internals/core-library-runtime-boundary.md).

## Library map

- [Text, Bytes, and Path](text-bytes-and-paths.md)
- [Collections and JSON](collections-and-json.md)
- [Task composition](task-composition.md)
- [I/O and logging](io-and-logging.md)
- [Resource protocols and lexical cleanup](../language/memory-and-resources.md)

The language behavior of `scoped`, `defer`, and `Task` is defined in the
[memory and resource](../language/memory-and-resources.md) and
[async and task](../language/async-and-tasks.md) references.

## Numeric conversion

### Float

```loom
import std.float.parse_float
import std.float.format_float
import std.float.is_finite
```

```text
parse_float(Text) Result[Float, ParseFloatError]
format_float(Float) Text
is_finite(Float) Bool
```

`ParseFloatError` is a closed value:

```text
InvalidSyntax
OutOfRange
```

`parse_float` consumes the complete Text. It accepts an optional leading `-`
and the decimal Float grammar described below; it does not accept whitespace,
numeric separators, or an integer-only spelling such as `"1"`.

```text
digits '.' digits [exponent]
digits exponent
exponent = ('e' | 'E') ['+' | '-'] digits
```

It also accepts exactly `"NaN"`, `"Infinity"`, and `"-Infinity"`. A decimal
whose magnitude overflows to infinity produces `OutOfRange`. Other decimals
are rounded to binary64, including possible underflow to zero.

`format_float` returns a shortest round-tripping decimal for finite values and
keeps integral finite values lexically distinct from Int by including a decimal
point or exponent. The special spellings are `NaN`, `Infinity`, and
`-Infinity`; negative zero formats as `-0.0`.

`is_finite` is a pure contract predicate and returns false for NaN and either
infinity.

### Int

```loom
import std.int.parse_int
```

```text
parse_int(Text) Result[Int, ParseIntError]
```

`ParseIntError` has the closed variants `InvalidSyntax` and `OutOfRange`.
Parsing consumes the complete Text as a decimal integer with an optional `+`
or `-` sign. Whitespace, separators, radix prefixes, and suffixes are rejected.
A syntactically valid integer outside the signed 64-bit range produces
`OutOfRange`.

## Process values

```loom
import std.process.arguments
import std.process.environment
```

```text
arguments() List[Text]
environment(name Text) Option[Text]
```

`arguments` returns the program arguments supplied to the Loom entry point in
their original order. `environment` returns `Some(value)` when the named
environment variable is present and representable as Text, otherwise `None`.

The library does not expose a mutable process-environment operation.

## Time values

```loom
import std.time.milliseconds
```

```text
milliseconds(Int) Duration
duration.as_milliseconds() Int
```

The input must be non-negative. A negative input produces RuntimeFault code
`InvalidDuration`. `Duration` is an immutable millisecond duration and supports
value equality. It can be passed to `Task.sleep`.

## Error and fault boundary

Expected parse, decode, path, JSON, and recoverable I/O rejection uses concrete
`Result` error types. These types are closed where exhaustive matching is
useful; there is no shared `Error` base class or implicit error conversion.

Contract faults, checked arithmetic faults, allocation failure, and operations
documented as faulting remain outside Result. See
[Diagnostics and failures](../language/diagnostics.md).
