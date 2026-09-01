# Text, Bytes, and Path

> Normative for Loom language version 0.4.

`Text`, `Bytes`, and `Path` have value semantics. Their storage address,
capacity, sharing, and allocation strategy are not observable. `Text` and
`Path` have no mutating operation. A mutable `Bytes` binding may be updated by
`add`, but logical copies remain independent.

The error types named below are ordinary compiler-distributed source enums:

```loom
import std.text.DecodeTextError
import std.path.PathError
```

Import them when their names or variants are used explicitly. They follow
normal enum resolution and carry no public builtin-type or builtin-variant
alias.

## `Text`

`Text` is a sequence of Unicode scalar values. Source literals always construct
valid Text. Text is not implicitly normalized and has no locale, case-folding,
grapheme-cluster, or mutable-buffer semantics.

```text
text.length() Int
text.get(index Int) Option[Text]
text.concat(other Text) Text
text.contains(needle Text) Bool
text.encode_utf8() Bytes
```

`DecodeTextError` is the closed public enum declared by `std.text`; its only
current variant is `InvalidUtf8`.

`length` counts Unicode scalar values, not UTF-8 bytes or user-perceived
grapheme clusters. `get` indexes the same scalar sequence and returns a
single-scalar Text. A negative or out-of-range index returns `None`.

`concat` returns the exact scalar sequence of the receiver followed by the
argument. `contains` performs an exact, case-sensitive subsequence search. It
does not normalize either operand or apply locale-specific rules.

`encode_utf8` returns the standard UTF-8 byte sequence for the Text.

Text equality compares scalar sequences. Since UTF-8 encoding is canonical for
Unicode scalar values, this is also equivalent to comparing their UTF-8 bytes.

## `Bytes`

`Bytes` is an arbitrary immutable byte sequence. It need not contain valid
UTF-8.

```text
bytes.length() Int
bytes.get(index Int) Option[Int]
bytes.append(other Bytes) Bytes
bytes.add(unit Int)
bytes.decode_utf8() Result[Text, DecodeTextError]
```

Source that constructs Text from integer units builds a mutable Bytes value,
checks each unit when its input is not already byte-ranged, appends with `add`,
and finishes with `decode_utf8`. An invalid UTF-8 sequence returns
`DecodeTextError.InvalidUtf8`; `add` requires every unit to be in `0..255` and
raises `InvalidByte` before mutation when that precondition is violated. There
is no parallel `List[Int]` conversion builtin.

`get` returns an Int in the range 0 through 255. A negative or out-of-range
index returns `None`. `append` returns the receiver bytes followed by the
argument bytes. `add` appends one unit to the receiver and requires a `var`
binding:

```loom
var bytes = "Loom".encode_utf8()
bytes.add(10)
```

The unit must be in the closed range 0 through 255. Any other value raises the
uncatchable RuntimeFault `InvalidByte` with message
`Bytes.add value is outside 0...255`; the receiver is not changed. Bytes use
copy-on-write value semantics, so updating one binding never changes another
logical copy. Capacity and whether an append reused storage are unobservable.

`decode_utf8` validates the complete sequence. Success produces Text; any
invalid sequence produces:

```text
DecodeTextError.InvalidUtf8
```

There is no implicit Text/Bytes conversion.

Bytes equality compares length and byte contents.

## `Path`

`Path` is a portable lexical path value. Its separator is `/` on every target.
It does not inspect a file system, normalize `.` or `..`, collapse repeated
separators, or assign platform-specific meaning to reverse solidus or drive
spellings.

```text
Path.from_text(text Text) Result[Path, PathError]
path.as_text() Text
path.join(child Path) Result[Path, PathError]
```

`PathError` is the closed public enum declared by `std.path`:

```text
ContainsNul
AbsoluteJoin
```

`from_text` rejects a Text containing U+0000 with `ContainsNul`; every other
Text is retained exactly. A path is absolute when its Text begins with `/`.
The backing Text field is not source-visible: raw record construction and field
mutation are rejected, so these constructors are the only way to establish a
Path value.

`base.join(child)` rejects an absolute child with `AbsoluteJoin`. Otherwise it
returns a lexical concatenation:

- if `base` is empty, the result is `child`;
- if `base` already ends in `/` or `child` is empty, it concatenates directly;
- otherwise it inserts one `/` between them.

`as_text` returns the exact lexical spelling. Path equality compares that
spelling.

Path is a lexical value containing an exact Text spelling, not an open
filesystem object or platform handle. Its API does not query a filesystem,
canonicalize components, embed JSON policy, or introduce ownership or borrowing
requirements. Allocation, sharing, and collection remain unobservable and no
part of this API defines a public FFI representation.
