# Collections and JSON

> Normative for Loom language version 0.3.

## `List[T]`

A list is an ordered homogeneous sequence:

```loom
let values = [1, 2, 3]
var empty = List[Int]()
```

List literals infer one common element type. An empty list literal needs an
expected `List[T]` type; `List[T]()` is the explicit empty constructor.

```text
list.length() Int
list.get(index Int) Option[T]
list.add(value T)
```

`length` returns the element count. `get` returns a logical copy of the element
at a zero-based index, or `None` for a negative or out-of-range index.

`add` appends one element and requires a `var` receiver:

```loom
var names = List[Text]()
names.add("Loom")
```

Lists have value semantics. Mutating one list value does not mutate another
logical copy. List equality is available when `T` supports value equality and
compares elements in order.

A `MustScope` resource cannot be placed in a List. Lists of Task values are
permitted for structured joins, but the complete task obligation must be
transferred according to the
[task rules](../language/async-and-tasks.md#dynamic-task-collections).

## `TextMap[V]`

`TextMap[V]` is a Text-keyed value map:

```loom
let empty = TextMap[Int]()
let one = empty.insert("answer", 42)
```

```text
map.length() Int
map.contains(key Text) Bool
map.get(key Text) Option[V]
map.entry_at(index Int) Option[(Text, V)]
map.insert(key Text, value V) TextMap[V]
map.remove(key Text) TextMap[V]
list.to_text_map() Result[TextMap[V], Text]
```

`insert` returns a new map and replaces the previous value when the key already
exists. `remove` returns a new map and is unchanged when the key is absent. The
original map remains observably unchanged, so these methods do not require a
`var` receiver.

`to_text_map` performs one bulk construction from a List of key/value pairs. It
sorts keys once by lexicographic order of their UTF-8 encoding instead of
lowering to repeated `insert` calls. Success returns `Ok` with the canonical
map. When any key occurs more than once, it returns `Err` with the
lexicographically smallest duplicated Text key. The input List remains
observably unchanged under normal value semantics.

Map keys have a canonical order: lexicographic order of their UTF-8 encoding.
This order determines `entry_at`, canonical JSON object output, and structured
logging. `entry_at` returns a logical copy of the key/value pair at a zero-based
canonical-order index, or `None` for a negative or out-of-range index. It does
not expose insertion history or a runtime-specific storage order.

Map equality is available when `V` supports value equality. It compares the
same key/value mapping, independent of the history of inserts and removals.
A `MustScope` value cannot be inserted into a TextMap. Operations that would
partially extract or transfer a Task-carrying map value are also rejected.

## `Json`

`Json` is a closed recursive value:

```text
Json.Null
Json.Bool(Bool)
Json.Number(Float)
Json.Text(Text)
Json.Array(List[Json])
Json.Object(TextMap[Json])
```

Construction examples:

```loom
let value = Json.Object(
    TextMap[Json]().insert("answer", Json.Number(42.0))
)
```

`Json.Null` is a value, not a call. The other variants shown with payloads are
static constructors. A Json match may use short expected-type patterns:

```loom
match value {
    Null => Unit
    Bool(flag) => { discard flag }
    Number(number) => { discard number }
    Text(text) => { discard text }
    Array(items) => { discard items }
    Object(fields) => { discard fields }
}
```

## Parsing and formatting JSON

```loom
import std.json.parse_json
import std.json.format_json
```

```text
parse_json(Text) Result[Json, JsonError]
format_json(Json) Result[Text, JsonError]
```

`JsonError` is closed:

```text
InvalidSyntax(offset Int)
NumberOutOfRange(offset Int)
DepthLimit
NonFiniteNumber
```

Offsets are zero-based UTF-8 byte offsets from the beginning of the input.

Parsing consumes one complete JSON document, permitting only JSON whitespace
around it. It rejects invalid escapes and surrogate sequences, trailing input,
leading-zero number forms, duplicate object keys, and non-finite or out-of-range
numbers. Object names are decoded before duplicate selection. A duplicate
reports `InvalidSyntax` at that key's second source occurrence; if multiple
distinct keys are duplicated, the selected key is the lexicographically
smallest duplicated key under canonical UTF-8 ordering. Container nesting
deeper than 128 produces `DepthLimit`.

`parse_json` is an ordinary function in the compiler-owned `std` source
package. Its helper graph is type-checked, lowered to direct calls and typed
collection operations, and removed by normal reachability when unused. There
is no JSON-parser builtin, MIR opcode, runtime entry point, or runtime type
switch.

Formatting emits canonical compact JSON with no unnecessary whitespace.
Object keys use TextMap canonical order, strings use JSON escaping, and finite
numbers use a shortest decimal representation. `Json.Number` can hold any Float
value, but formatting NaN or infinity produces `NonFiniteNumber`. Formatting
also applies the 128-container depth limit.

`format_json` is a compiler-known typed operation over the exact closed `Json`
and `JsonError` shapes. Its runtime boundary receives a compiler-supplied
layout descriptor rather than a universal value or source type identifier.

Json equality is recursive value equality. Object equality compares mappings,
not insertion history.
