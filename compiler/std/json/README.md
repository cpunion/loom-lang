# JSON

`std.json` is implemented in Loom; it adds no JSON behavior to the runtime.
`parse(Text)` returns a `Result[Value, ParseError]`, and `stringify(Value)`
returns a `Result[Text, WriteError]`.

`encode[T](value T)` returns a `Result[Text, WriteError]` without building a
`Value` tree first. It supports Bool, Int, finite Float, Text, Lists, tuples,
visible records, their supported refinements, and embedded `Value` values.
Records use declared field names/order; Lists and tuples become arrays. Type
selection and field iteration happen at compile time, with no runtime schema
registry. See the [runnable example](../../examples/json_encoding/main.loom).

Private types from another package are opaque. Enums other than `Value`, Bytes,
callbacks and other unsupported types return `WriteError.UnsupportedType`; no
enum tagging, binary encoding or hidden-field exposure is guessed. Task and
MustScope obligations still apply at checking. Non-finite Floats return
`WriteError.InvalidNumber`. This API does not provide typed decoding or custom
field mappings yet; use `Value` for an explicit wire representation.

`Value.Number` stores a validated JSON number spelling as `Text`, so parsing
does not round large integers or decimals through `Float`. A manually built
number is validated when written. Objects retain source field order and reject
duplicate decoded keys both when parsed and when written. Strings decode JSON
escapes, including UTF-16 surrogate pairs, into Loom's UTF-8 `Text`.

Both directions cap array/object nesting at 64 levels; this also makes a
manually constructed cyclic `Value` return `WriteError.DepthExceeded` instead
of recursing forever. `encode` uses the same limit, including recursive records
with shared Lists. These are in-memory APIs, not streaming parsers or writers.
