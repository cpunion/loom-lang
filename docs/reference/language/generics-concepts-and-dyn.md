# Generics, concepts, and `dyn`

> Normative for Loom language version 0.3.

Loom uses one abstraction mechanism for behavior: a nominal `concept` with an
explicit conformance. The same declaration supports generic bounds, interface
parameters, and, when declared dynamic-compatible, first-class `dyn` values.

## Generic declarations

Records, enums, functions, methods, and implementation blocks may declare
rank-1 type parameters in square brackets:

```loom
record Pair[A, B] {
    first A
    second B
}

fn choose[T](first T, second T, use_first Bool) T {
    if use_first { first } else { second }
}
```

Type arguments are invariant. A generic body is checked at its definition,
using only operations justified by its declared bounds. An unconstrained `T`
may be stored, passed, returned, and placed in other values, but it does not
support arbitrary methods, arithmetic, ordering, or equality.

Calls infer type arguments from arguments and the expected result type. An
explicit call uses `function[A, B](...)`. Generic type applications use the
same brackets, for example `Pair[Int, Text]`.

There are no default type arguments, variadic type parameters, higher-kinded
types, specialization, or type-level computation.

## Concepts and bounds

A concept declares required behavior:

```loom
dyn concept Display {
    method display(self) Text
}

concept Zero {
    static method zero() Self
}

concept Comparable {
    method less_equal(self, other Self) Bool
}
```

Concept declarations do not have their own type parameters. A requirement may
have method-specific generic parameters when the concept is not declared
dynamic-compatible.

Bounds follow a generic parameter after a colon. Multiple bounds use `+`:

```loom
fn render[T: Display](value T) Text {
    value.display()
}

fn inspect[T: Display + Comparable](value T) Text {
    value.display()
}
```

The colon is specific to bounds; ordinary parameters and fields use
`name Type`.

## Explicit conformance

A type conforms only through an `impl C for T` declaration:

```loom
record Label {
    text Text
}

impl Display for Label {
    method display(self) Text {
        self.text
    }
}
```

The implementation must provide every required method and associated type
exactly once, with the exact signature obtained after substituting `Self` and
associated types. It cannot add unrelated methods; those belong in an inherent
`impl T` block. A conformance method inherits its requirement's contracts and
must not repeat them.

Conformance is nominal, not structural. A matching method name is insufficient
without the explicit `impl`.

The conformance must be declared in the package that owns the concept or the
outer nominal target type. For a given target and concept, applicable
conformances must not overlap. Import order and link order never select between
implementations.

## Conditional conformance

An implementation may depend on bounds for type parameters determined by its
target:

```loom
record Boxed[T] {
    value T
}

impl[T: Display] Display for Boxed[T] {
    method display(self) Text {
        self.value.display()
    }
}
```

Each implementation parameter must be determined by the target head. A
prerequisite must apply to a strict structural subterm of that target. These
rules reject unconstrained parameters, overlapping heads, and recursive proof
search.

## Associated types

A concept may declare associated types, optionally with bounds:

```loom
dyn concept Source {
    associated type Item: Display
    method read(self) Self.Item
}
```

A conformance binds each one:

```loom
record Labels {
    label Label
}

impl Source for Labels {
    associated type Item = Label

    method read(self) Label {
        self.label
    }
}
```

The bound on an associated type is checked against the binding. Cyclic
associated bindings are rejected.

Within a concept, `Self.Item` names its associated type. A uniquely bounded
generic parameter may use `T.Item`. The fully qualified form
`<T as Source>.Item` removes ambiguity.

## Method lookup and qualification

Dot lookup considers inherent methods and methods justified by the receiver's
declared concept bounds or interface type. A generic body cannot acquire a
method merely because some conformance happens to exist elsewhere in the
program.

When more than one concept supplies the same name, call the requirement in
qualified form:

```loom
<T as Comparable>.less_equal(left, right)
```

Static requirements use the same form, for example `<Int as Zero>.zero()`.
Selecting a qualified method without immediately calling it is not a value.

## Interface parameters

A dynamic-compatible concept name is concise interface syntax in parameter
position:

```loom
dyn concept Formatter {
    associated type Error
    method format(self, text Text) Result[Text, Self.Error]
}

fn format_with(
    formatter Formatter[Error = Text],
    text Text,
) Result[Text, Text] {
    formatter.format(text)
}
```

At a call site, a concrete value with the required conformance adapts
automatically. Parameter spellings `formatter Formatter[...]` and
`formatter dyn Formatter[...]` have the same observable call semantics. The
short form exists so callers and API authors rarely need to choose between
static and dynamic notation for a one-call interface use.

For a synchronous call, if the concept has a `mut self` requirement, the
concrete argument must be a `var` place. Mutations made through the parameter
are written back to that place on normal return. This interface access cannot
overlap an incompatible read or write.

An async function instead receives an independent logical value for an
interface parameter. Calling it does not require a `var` argument and mutations
inside the async body are not written back to the caller. A synchronous
write-back access never remains active across `.await`.

## First-class `dyn` values

Outside parameter position, an interface value is written explicitly as
`dyn C`:

```loom
record Renderer {
    value dyn Display
}

fn erase_label(value Label) dyn Display {
    value
}
```

Only a `dyn concept` may be used this way. Every associated type must be bound:

```loom
dyn Source[Item = Label]
```

A dynamic-compatible concept must contain only receiver methods. It cannot
contain a static requirement or a method-specific generic parameter. `Self`
may appear as the receiver and through the concept's associated projections,
but cannot otherwise appear in a parameter or return type.

A `dyn C` value is a normal first-class value: it can be stored in records,
enums, tuples, lists, parameters, and results, copied, and returned. Copying it
copies the logical concrete value. A later `mut self` call on one copy does not
mutate another copy, and such a call requires a `var` receiver.

Conversion to `dyn C` selects a statically known conformance. Loom performs no
runtime search by concrete type or method name. There is no universal `any`,
downcast, reflection query, or runtime conformance registry.

Type erasure cannot hide static cleanup obligations. A value that directly or
recursively contains a `MustScope` resource or live `Task` cannot convert to
`dyn C`. A generic source whose complete shape cannot prove the absence of
those obligations is rejected as well. This rule applies at the conversion,
before the concrete obligation could be lost.

Programs can observe only the value, dispatch, mutation, and fault behavior of
`dyn C` described above. Its representation has no source-level identity.

## Current compiler representation

The native compiler closes each dynamic view over witnesses reachable from the
artifact being built. A single exact closed nongeneric witness is erased to
its concrete value and all calls are direct. A finite set of two or more exact
closed nongeneric witnesses uses one managed pointer to a compiler-private
candidate box. The box has a private ordinal tag and that candidate's exact
payload; dispatch is a finite switch to direct methods. Records, enums, tuples,
and Lists store that one pointer.

Readonly copies may share the immutable box. A mutable call never changes a
published box in place: it receives the concrete method writeback, creates a
fresh box, and updates only the selected `var` place. This preserves the value
copy rule above even when the collector moves objects.

This representation is not a stable cross-artifact ABI. It contains no witness
pointer, universal value, runtime conformance registry, or source-visible type
tag. Missing witnesses and open, generic, prerequisite-dependent, or otherwise
incomplete candidate sets currently fail closed for typed LCIR and may select
the complete checked-MIR native route; the compiler never guesses a finite catalog.
