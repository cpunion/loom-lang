# Packages and declarations

> Normative for Loom language version 0.3.

## Source-file structure

A source file contains imports followed by top-level declarations:

```loom
import shop.currency.Currency
import std.float.is_finite

pub type Price = Float where is_finite(self) && self >= 0.0
```

A source file has no package or module declaration. It cannot contain
executable top-level statements or mutable global variables. `module` is an
ordinary identifier, not a reserved word.

## Directory packages

The filesystem determines package membership. The directory containing
`loom.toml` is the root of a named, versioned module and is also its root
package. A source directory below it extends the package path by its relative
directory segments:

```text
shop/                       module shop, package shop
├── loom.toml
├── checkout.loom
└── currency/               package shop.currency
    ├── amount.loom
    └── format.loom
```

Every source directory segment matches `[a-z][a-z0-9_]*`. File names do not
contribute to the package path. All `.loom` files in one directory form one
package and share its declaration namespace. Declaration order and file
discovery order do not affect name resolution.

The nearest ancestor `loom.toml` owns a source file. A nested manifest starts a
separate module boundary, and outer source discovery does not cross it. There
is no distinguished `src/` directory and no manifest source-root list.

## Package and import paths

A package path is a dot-separated sequence of identifiers. An import names one
declaration by its fully qualified path and introduces that declaration's final
name into the importing package:

```loom
import shop.currency.Currency
import shop.pricing.calculate_total
```

There are no wildcard, grouped, or alias imports. Importing a declaration does
not execute initialization and does not register implementations at runtime.

Imports participate in a closed package graph. Cycles are rejected statically.
The first path segment names the root module or one of its direct dependency
aliases. The alias `std` is reserved for the read-only compiler-owned standard
library module.

## Package initialization

Importing a package never executes user code. Loom 0.3 has no `init`
declaration or block, no executable top-level statements, no mutable globals,
and no user-defined top-level constant syntax. A function named `init` is an
ordinary function and is never discovered or called implicitly.

Applications perform runtime initialization through ordinary functions called
explicitly from `main`. Reusable packages expose those functions without
creating an implicit initialization order. A future constant declaration may
be admitted only when its value can be evaluated completely at compile time;
it must not introduce hidden runtime work. Process-wide lazy values belong in
future standard-library abstractions such as `Lazy` and `Once`, whose use
remains explicit in the package graph.

The compiler and runtime may establish fixed facilities such as the GC,
executor, or Runtime ABI before invoking the program entry point. That is an
internal toolchain contract, not a user-extensible package initialization
mechanism, and it does not make imports effectful.

## Visibility

Declarations are private to their package unless prefixed by `pub`. Files in
the same directory can use one another's private declarations. Any use from a
different directory package, including another package in the same module,
requires a public declaration and an explicit import.

Public visibility is available for constrained types, records, enums,
functions, async functions, concepts, and dynamic concepts:

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

A private declaration cannot be named from another package through a qualified
path. Such a use is a `NameNotVisible` error.

## Top-level declarations

After imports, an ordinary `.loom` file may contain these declaration forms:

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
```

A file whose name ends in `_test.loom` may additionally contain:

```text
test fn name() { ... }
test async fn name() { ... }
```

Test declarations are rejected in every other file. Production compilation
excludes all `*_test.loom` files. `loom test` adds only test files belonging to
the selected root module; dependency test files are neither loaded nor run.

The bracketed parts above denote optional generic parameter lists; they are not
literal ellipses in Loom source. Concept declarations themselves do not accept
type parameters.

Signatures are resolved package-wide before bodies are checked, so a
declaration may refer to another declaration that appears later or in another
file in the same package.

Duplicate declarations in the same namespace are rejected. Type, value, and
concept lookup are distinct where the language needs them, but duplicate data
members, generic parameters, methods, variants, and associated types are also
rejected within their owner.

## Ownership of methods and conformances

An inherent implementation must be declared in the package that owns its
target nominal type:

```loom
impl Invoice {
    pub method total_text(self) Text {
        discard self
        "invoice"
    }
}
```

A conformance `impl C for T` must be declared by the package that owns `C` or
by the package that owns the outer nominal type `T`. This orphan rule, together
with overlap checks, makes conformance selection independent of imports and
link order. See [Generics, concepts, and `dyn`](generics-concepts-and-dyn.md).
