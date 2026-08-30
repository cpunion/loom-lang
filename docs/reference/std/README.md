# Standard library reference

> Normative for Loom language version 0.3.

The standard library is deliberately small. Its value types follow the same
static typing, value semantics, contracts, resource obligations, and task rules
as user declarations. Standard functions are available only after an explicit
import; standard value types and their built-in constructors and methods are in
the prelude.

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
`std.text.DecodeTextError`; `std.path.PathError`; `std.io.write` and
`write_line`; the `std.log.debug`, `info`, `warn`, and `error` conveniences;
and the public `Dispose`, `MustScope`, and `NoSuspend` declarations in
`std.resource`.
The logging conveniences are ordinary Loom functions over the irreducible
`std.log.write` output boundary. Resource declarations are source-backed, but
their fixed shapes and irreducible static rules remain part of the language
core and add no runtime registry. JSON parsing is ordinary Loom source; JSON
formatting uses the exact typed formatting boundary documented in the JSON
reference. The target boundary permits only irreducible GC, scheduler,
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
infinity.

All three functions are ordinary source definitions. Their exact-owner private
scalar calls expose only `(Float, Int)` parse status, managed Text formatting,
and one finite Boolean; the compiler never constructs `ParseFloatError`.

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
and error policy are ordinary `std.float` Loom source. The private scalar
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
