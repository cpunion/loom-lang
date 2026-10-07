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
`WriteError.InvalidNumber`.

`decode[T](text Text)` returns a `Result[T, DecodeError]`. It first parses a
`Value` tree, then generates ordinary typed construction for Bool, Int, finite
Float, Text, Lists, tuples and visible records. Recursive records work through
Lists; `Value` itself retains the parsed representation. Record objects must
contain exactly their declared fields, in any input order; tuple arrays must
have exactly their declared length. Missing fields, extra fields and wrong
value kinds reject. Int decoding checks the decimal spelling and range directly,
without a Float round trip; `1.0` is not an Int spelling.

Decoding does not guess defaults, null/enum representations or custom field
mappings. Refined types return `DecodeError.UnsupportedType`, as do foreign
private types and other unsupported kinds. Decode an unconstrained wire record,
then use normal checked construction for constrained application types. This
keeps invariant checks at their explicit boundaries, including nested fields.
Malformed input returns `DecodeError.Syntax(ParseError)`.

`Value.Number` stores a validated JSON number spelling as `Text`, so parsing
does not round large integers or decimals through `Float`. A manually built
number is validated when written. Objects retain source field order and reject
duplicate decoded keys both when parsed and when written. Strings decode JSON
escapes, including UTF-16 surrogate pairs, into Loom's UTF-8 `Text`.

Both directions cap array/object nesting at 64 levels; this also makes a
manually constructed cyclic `Value` return `WriteError.DepthExceeded` instead
of recursing forever. `encode` uses the same limit, including recursive records
with shared Lists. Typed decoding uses the parser's same nesting limit. These
are in-memory APIs.

`decoder()`, `feed(decoder, Bytes)` and `finish(decoder)` parse one document
incrementally. Arbitrary chunk boundaries may split UTF-8, escapes or numbers.
Only the current scalar token and the growing `Value` tree are retained, not
the full input text. Completed tokens reuse `parse` for the same scalar rules;
container grammar carries across feeds. `feed` reports consumed bytes, while
`finish` requires EOF and returns the value. Errors retain absolute byte offsets
and stick across later calls; finish is terminal. Decoder copies share a cursor,
so serialize feeds. The same duplicate-key and 64-level nesting policy applies.

[`std.json.stream`](stream/README.md) awaits an existing Stream and uses this
decoder. It needs no JSON runtime or new producer protocol.

`encoder(value, chunk_size)` and `next(encoder)` write a `Value` incrementally.
The chunk size must be positive. Each item is a fresh nonempty `Bytes` of at most
that size; UTF-8 and escapes may split across chunks. Concatenate bytes before
decoding Text. EOF is `None`; invalid numbers, duplicate keys or excessive depth
produce one error item, then EOF. Earlier chunks are not rolled back.

Encoder copies share a serialized cursor. Open containers retain fixed-shape
List views, not an isolated snapshot of the entire graph: shared element writes
and not-yet-opened children remain observable. Synchronize application mutations
when a consistent document is required. Encoding retains traversal state, key
sets and the current output chunk, not a complete serialized document.
The async writer uses the same cursor. Typed streaming encoding and
record-at-a-time document sequences are not provided.
