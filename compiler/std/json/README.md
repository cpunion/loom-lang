# JSON

`std.json` is implemented in Loom; it adds no JSON behavior to the runtime.
`parse(Text)` returns a `Result[Value, ParseError]`, and `stringify(Value)`
returns a `Result[Text, WriteError]`.

`Value.Number` stores a validated JSON number spelling as `Text`, so parsing
does not round large integers or decimals through `Float`. A manually built
number is validated when written. Objects retain source field order and reject
duplicate decoded keys both when parsed and when written. Strings decode JSON
escapes, including UTF-16 surrogate pairs, into Loom's UTF-8 `Text`.

Both directions cap array/object nesting at 64 levels; this also makes a
manually constructed cyclic `Value` return `WriteError.DepthExceeded` instead
of recursing forever. This is an in-memory value API, not a streaming parser or
automatic record-to-JSON mapping.
