# Modules and declarations

> Normative for Loom language version 0.3.

## Source-file structure

Every source file begins with exactly one `module` declaration:

```loom
module shop.pricing

import shop.currency.Currency
import std.float.is_finite

pub type Price = Float where is_finite(self) && self >= 0.0
```

Imports follow the module declaration and precede every other top-level
declaration. A source file cannot contain executable top-level statements or
mutable global variables.

One module may be formed from more than one source file. The module name, not a
file name or discovery order, determines the declaration namespace. All files
that contribute to a module share its declarations and imports.

## Module and import paths

A module path is a dot-separated sequence of identifiers. An import names one
declaration by its fully qualified path and introduces that declaration's final
name into the importing module:

```loom
import shop.currency.Currency
import shop.pricing.calculate_total
```

There are no wildcard, grouped, or alias imports. Importing a declaration does
not execute initialization and does not register implementations at runtime.

Imports participate in a closed module graph. A cycle in that graph is a static
`ModuleCycle` error.

The first path segment `std` is reserved for the read-only compiler-owned
standard-library package.

## Visibility

Declarations are private to their module unless prefixed by `pub`. Public
visibility is available for constrained types, records, enums, functions,
async functions, concepts, and dynamic concepts:

```loom
pub record Invoice {
    total Float
}

pub async fn load_invoice(id Text) Invoice {
    discard id
    Invoice { total = 0.0 }
}
```

Methods inside an inherent `impl` may also be `pub`. Conformance
implementations are declarations of a relationship, not independently public
members. An `impl` block and a `test fn` cannot be prefixed by `pub`.

A private declaration cannot be named from another module, including through a
qualified path. Such a use is a `NameNotVisible` error.

## Top-level declarations

After imports, a module may contain these declaration forms:

```text
(pub)? type Name = Base where predicate
(pub)? record Name[...] { ... }
(pub)? enum Name[...] { ... }
(pub)? fn name[...] (...) Return { ... }
(pub)? async fn name[...] (...) Return { ... }
(pub)? concept Name { ... }
(pub)? dyn concept Name { ... }
impl[...] Type { ... }
impl[...] Concept for Type { ... }
test fn name() { ... }
test async fn name() { ... }
```

The bracketed parts above denote optional generic parameter lists; they are not
literal ellipses in Loom source. Concept declarations themselves do not accept
type parameters.

Declaration order does not control name resolution. Signatures are resolved
module-wide before bodies are checked, so a declaration may refer to another
declaration that appears later in the module.

Duplicate declarations in the same namespace are rejected. Type, value, and
concept lookup are distinct where the language needs them, but duplicate data
members, generic parameters, methods, variants, and associated types are also
rejected within their owner.

## Ownership of methods and conformances

An inherent implementation must be declared in the module that owns its target
nominal type:

```loom
impl Invoice {
    pub method total_text(self) Text {
        discard self
        "invoice"
    }
}
```

A conformance `impl C for T` must be declared by the module that owns `C` or by
the module that owns the outer nominal type `T`. This orphan rule, together with
overlap checks, makes conformance selection independent of imports and link
order. See [Generics, concepts, and `dyn`](generics-concepts-and-dyn.md).
