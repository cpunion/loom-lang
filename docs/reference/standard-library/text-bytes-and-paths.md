# Text, Bytes, and Path

> Normative for Loom language version 0.3.

`Text`, `Bytes`, and `Path` are immutable values. Their storage address,
capacity, sharing, and allocation strategy are not observable.

## `Text`

`Text` is a sequence of Unicode scalar values. Source literals always construct
valid Text. Text is not implicitly normalized and has no locale, case-folding,
grapheme-cluster, or mutable-buffer semantics.

```text
Text.from_utf8_units(units List[Int]) Result[Text, DecodeTextError]
text.length() Int
text.get(index Int) Option[Text]
text.concat(other Text) Text
text.contains(needle Text) Bool
text.encode_utf8() Bytes
```

`from_utf8_units` is the explicit low-level construction boundary used by
source libraries that decode external formats. Every Int must be in the range
0 through 255 and the complete sequence must be valid UTF-8. Success produces
Text; an out-of-range unit or malformed sequence produces
`DecodeTextError.InvalidUtf8`. The conversion is never implicit, and it does
not define JSON or any other data-format policy.

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
bytes.decode_utf8() Result[Text, DecodeTextError]
```

`get` returns an Int in the range 0 through 255. A negative or out-of-range
index returns `None`. `append` returns the receiver bytes followed by the
argument bytes.

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

`PathError` is closed:

```text
ContainsNul
AbsoluteJoin
```

`from_text` rejects a Text containing U+0000 with `ContainsNul`; every other
Text is retained exactly. A path is absolute when its Text begins with `/`.

`base.join(child)` rejects an absolute child with `AbsoluteJoin`. Otherwise it
returns a lexical concatenation:

- if `base` is empty, the result is `child`;
- if `base` already ends in `/` or `child` is empty, it concatenates directly;
- otherwise it inserts one `/` between them.

`as_text` returns the exact lexical spelling. Path equality compares that
spelling.
