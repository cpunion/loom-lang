# Standard library reference

> Normative for Loom language version 0.4.

The standard library is deliberately small. Its value types follow the same
static typing, value semantics, contracts, resource obligations, and task rules
as user declarations. Standard functions are available only after an explicit
import. A deliberately small set of standard value types is in the prelude;
their constructors and methods retain their ordinary declaration identities and
visibility.

The compiler-owned source module is `std`. Source imports library APIs by
their `std.*` path.

The standard library's public behavior is tested in Loom itself. From the
repository root, run:

```sh
loom test library/std/tests
```

These tests use the same compiler and runtime path as application tests. Rust
tests remain for ABI and compiler boundaries rather than duplicating ordinary
library behavior.

Source-backed packages are distributed as Loom source and compile through the
ordinary package, type, MIR, reachability, and native pipelines. Current source
declarations include the `std.int`, `std.float`, and `std.json` parsers and
their public errors; the complete `std.float` public API and conversion error;
`std.text.DecodeTextError`; `std.path.PathError`; `std.io.IoError`,
`IoErrorKind`, the record's fields and convenience methods, `write`, and
`write_line`; the complete public
`std.log` API, including `LogLevel` and `write`; all public `std.file`
open/create APIs, `File`, its I/O methods and resource conformances; all public
`std.net` connect APIs, `Socket`, its I/O methods and resource conformances; and
the public `Dispose`, `MustScope`, and `NoSuspend` declarations in
`std.resource`.
`std.json.parse_json` and `std.json.format_json` are ordinary source functions;
their parser, traversal, escaping, and result construction use general Loom
control flow and collection/Text operations rather than JSON-specific compiler
or runtime hooks.
`std.time.Duration`, `milliseconds`, and `Duration.as_milliseconds` are
ordinary source declarations. `Duration` is a constrained `Int`; the compiler
does not provide a duration prelude identity, constructor, layout, or
inspection intrinsic.
`Task.sleep` is an ordinary public static method in `std.task`. Its source body
alone may call the exact-owner private `__sleep` timer leaf; callers have no
public-name intrinsic or compatibility fallback.
`Path.from_text`, `Path.as_text`, and `Path.join` are ordinary public methods in
`std.path`. Their three exact source bodies alone may call the corresponding
private typed leaves; callers likewise have no public-name fallback.
The public logging graph is ordinary Loom source over one compiler-private
typed output primitive. Resource declarations are source-backed, but
their fixed shapes and irreducible static rules remain part of the language
core and add no runtime registry. The target boundary permits only irreducible GC, scheduler,
platform, output, and generic construction services to cross into the
compiler-private runtime. The implementation rule is documented in
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
import std.float.FloatToIntError
import std.float.from_int
import std.float.to_int
```

```text
parse_float(Text) Result[Float, ParseFloatError]
format_float(Float) Text
is_finite(Float) Bool
from_int(Int) Float
to_int(Float) Result[Int, FloatToIntError]
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
infinity. Its ordinary source body uses ordered comparisons against the
positive and negative maximum finite binary64 values.

All three functions are ordinary source definitions. Exact-owner private
scalar calls remain only for parsing and formatting; the compiler never
constructs `ParseFloatError`.

`from_int` rounds the exact signed 64-bit integer to the nearest IEEE-754
binary64 value, with ties rounded to even. It always succeeds, but integers
outside binary64's exact-integer range can lose precision. Round-tripping is
therefore guaranteed only when the original Int is exactly representable; in
particular, maximum Int rounds to `2^63`, which `to_int` reports as
`OutOfRange`.

`to_int` truncates a finite Float toward zero. It returns `NonFinite` for NaN
or either infinity and `OutOfRange` when the truncated value cannot be
represented by signed 64-bit `Int`. `FloatToIntError` is an ordinary public
source enum with those two closed variants.

Both conversions are explicit: there is no implicit numeric conversion,
general cast syntax, or platform-sized numeric result. Their public functions
and error policy are ordinary `std.float` Loom source. The conversion
primitives have exact compiler-owned module authority, allocate nothing, and
require no runtime ABI.

### Int

```loom
import std.int.ParseIntError
import std.int.parse_int
```

```text
parse_int(Text) Result[Int, ParseIntError]
```

`ParseIntError` is an ordinary public enum declared by `std.int`, with the
closed variants `InvalidSyntax` and `OutOfRange`. Parsing consumes the complete
Text as a decimal integer with an optional `+` or `-` sign. Whitespace,
separators, radix prefixes, suffixes, and non-ASCII digits are rejected. A
syntactically valid integer outside the signed 64-bit range produces
`OutOfRange`; invalid syntax takes precedence when both conditions occur.

The implementation is ordinary Loom source over `Text.encode_utf8` and
`Bytes`. The compiler and runtime do not contain an integer-parser opcode,
builtin, or ABI entry point.

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

Both public functions are ordinary Loom source definitions. Their bodies call
compiler-private process primitives that are available only to the exact
compiler-owned `std.process` module; application and dependency source cannot
import those primitives. Consequently the wrappers and process runtime symbols
enter an artifact only through normal source reachability.

The library does not expose a mutable process-environment operation.

## Time values

```loom
import std.time.milliseconds
```

```text
milliseconds(Int) Duration
duration.as_milliseconds() Int
```

The input must be non-negative. `milliseconds` expresses that rule as a
`requires` clause, so a negative input produces the ordinary
`PreconditionFault`. `Duration` is declared in source as
`Int where self >= 0`; it is immutable, supports value equality, and can be
passed to `Task.sleep`. A proven direct `Duration(42)` construction has type
`Duration`, while a construction whose predicate is not proved has the normal
`Result[Duration, ConstraintError]` constrained-type form.

All three declarations use ordinary source resolution, reachability, contract
proof elimination, and dead-code elimination. There is no duration-specific
compiler primitive or runtime ABI.

## Error and fault boundary

Expected parse, decode, path, JSON, and recoverable I/O rejection uses concrete
`Result` error types. These types are closed where exhaustive matching is
useful; there is no shared `Error` base class or implicit error conversion.

Contract faults, checked arithmetic faults, allocation failure, and operations
documented as faulting remain outside Result. See
[Diagnostics and failures](../language/diagnostics.md).
