# Lexical structure

> Normative for Loom language version 0.3.

## Source text

A Loom source file is UTF-8 text. An optional Unicode byte-order mark is
accepted only as the first character of a file. A byte-order mark anywhere else
is an `InvalidSourceCharacter` error.

Space and tab are horizontal whitespace. Line endings may be LF, CRLF, or CR.
They have the same grammatical effect. Form feed and vertical tab are not
whitespace in Loom source.

## Identifiers

Identifiers follow Unicode XID rules:

- the first character is a Unicode XID start character;
- later characters are Unicode XID continuation characters;
- an underscore may start an identifier when another XID continuation
  character follows it;
- a single `_` is the wildcard token, not an identifier.

Identifiers are case-sensitive. `Price`, `price`, and `PRICE` are different
names.

The reserved words are:

```text
as associated assert async await break concept const continue defer discard dyn else
ensures enum false fn for if impl import in invariant let match method mut old
pub record requires result return scoped self static test true type var where while
```

`Self` and `Unit` are not keywords. They are names resolved by the type and
value namespaces. `module` is also an ordinary identifier; module membership is
defined by `loom.toml` and source directories, not by a source declaration.

## Comments

`//` begins a line comment. `///` begins a documentation-style line comment.
Both continue up to, but do not include, the next line ending.

```loom
// An ordinary comment.
/// A documentation-style comment before a declaration.
pub record Coordinate {
    x Float
    y Float
}
```

Loom has no block-comment syntax.

## Text literals

A text literal begins and ends with `"`. It cannot cross a physical line
ending.

```loom
let message = "Loom \u{1f642}\n"
```

The following escapes are accepted:

| Escape | Value |
| --- | --- |
| `\"` | quotation mark |
| `\\` | reverse solidus |
| `\/` | solidus |
| `\b`, `\f`, `\n`, `\r`, `\t` | JSON control escapes |
| `\0` | U+0000 |
| `\uXXXX` | one JSON UTF-16 code unit, with a required surrogate pair when needed |
| `\u{H...}` | one Unicode scalar from one to six hexadecimal digits |

A braced Unicode escape must name a Unicode scalar value. An isolated surrogate
in either Unicode escape form is rejected.

The type of a text literal is `Text`.

## Numeric literals

Loom uses decimal numeric literals only.

```text
Int:   0  12  9223372036854775807
Float: 1.0  1e3  2.0E-4
```

An `Int` literal is one or more ASCII decimal digits. Its value must fit `Int`.
The spelling `-9223372036854775808` is accepted as unary minus applied to the
one otherwise out-of-range magnitude needed to denote the minimum `Int`.

A `Float` literal has either:

- digits, `.`, and one or more digits, followed by an optional exponent; or
- digits followed by an exponent.

An exponent is `e` or `E`, an optional `+` or `-`, and one or more digits.
`.5`, `1.`, numeric separators, radix prefixes, and numeric suffixes are not
part of the grammar. Float literals must denote finite binary64 values; NaN and
infinity can arise through computation or standard-library parsing, but have no
source literal.

A leading `-` is a unary operator, not part of either literal token.

## Newlines and continuation

Newlines separate top-level declarations and items in a block. Loom does not
use semicolons.

A newline is suppressed while inside parentheses or brackets. It is also
suppressed after a token that necessarily continues the current construct:

```text
( [ { , . .. : = => + - * / == != < <= > >= && || !
```

This permits layouts such as:

```loom
let total = subtotal +
    tax

let pair = (
    total,
    "total",
)
```

Braces do not suppress all newlines in their contents. After the opening brace,
ordinary newlines separate block items. Record fields and invariants, enum
variants, record-literal fields, and match arms may be separated by either a
newline or a comma. Concept members, implementation members, top-level
declarations, and block items use newlines. Commas remain the separator in
parameter, argument, type-argument, tuple, and list forms.

## Punctuation and operators

The punctuation tokens are:

```text
( ) [ ] { } , . .. : = => + - * / == != < <= > >= && || ! ? _
```

`?` is the postfix `Result` propagation operator. `.await` is a postfix keyword
form; `await value` is not valid syntax.

## Syntactic nesting limit

One expression, type, or pattern may contain at most 128 nested syntactic
wrappers under nesting contract version 2. Atomic forms do not consume this
budget. Prefix operators, delimited forms, type and pattern payloads, calls,
member access, and other postfix wrappers do. Exceeding the limit is a
`SyntaxNestingLimit` source error.
