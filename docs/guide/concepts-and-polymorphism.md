# Concepts and polymorphism

`concept` is Loom's single behavior abstraction. Conformance is nominal and
explicit: a matching method name is not enough, imports do not activate an
implementation, and the runtime does not search for implementations.

The executable companion for this guide is
[`examples/concepts-polymorphism/concepts.loom`](../../examples/concepts-polymorphism/concepts.loom).

## Declare and implement a concept

```loom
dyn concept Labeled {
    method label(self) Text
}

record Label {
    text Text
}

impl Labeled for Label {
    method label(self) Text {
        self.text
    }
}
```

An implementation supplies every required method and associated type exactly
once, with the same signature after substituting `Self`. Additional inherent
behavior belongs in `impl Label`, not in the conformance body.

Coherence is closed and deterministic. An implementation must be declared in
the package that owns the concept or the outer nominal target type, and only one
implementation may apply to a concrete `(Type, Concept)` pair. Loom does not
support specialization, negative implementations, or link-order priority.

## Generic constraints

Use `T: C` when one concrete type identity should flow through an algorithm:

```loom
concept Ordered {
    method less_equal(self, other Self) Bool
}

fn smaller[T: Ordered](left T, right T) T {
    if left.less_equal(right) { left } else { right }
}
```

The body is checked only against declared bounds. A conformance that happens to
exist elsewhere in the package does not make undeclared methods available.
Multiple bounds use `+`, as in `T: Ordered + Labeled`.

The compiler may specialize a concrete call or pass a hidden proof. That choice
does not change source behavior.

## Interface parameters

When a function only needs a concept's behavior and does not preserve the
concrete type identity, use the concept directly in parameter position:

```loom
fn render(value Labeled) Text {
    value.label()
}
```

`value Labeled` and `value dyn Labeled` have the same observable parameter
semantics. The shorter spelling is idiomatic; callers pass a concrete conforming
value without constructing a separate interface value in source.

Only a concept declared with `dyn concept` may form an erased interface value:

```loom
dyn concept Formatter {
    associated type Error
    method format(self, text Text) Result[Text, Self.Error]
}
```

The leading `dyn` on the declaration promises that every requirement is safe
for erased dispatch. It does not create a second kind of concept.

## Stored and returned erased values

Outside parameter position, type erasure is explicit:

```loom
record LabelHolder {
    item dyn Labeled
}

fn erase(value Label) dyn Labeled {
    value
}
```

Use `dyn C` for a field, return type, enum payload, tuple/list element, or nested
generic argument. A bare `C` in those positions does not silently become an
erased value. This makes long-lived erasure visible in an API without adding
box, borrow, lifetime, or ownership syntax.

An erased value has ordinary Loom value semantics. Copying it copies the
underlying logical value while reusing immutable conformance proof data; a
later `mut self` call on one copy does not mutate another copy through a hidden
alias.

## Associated types

Associated types keep relationships inside a concept:

```loom
dyn concept Source {
    associated type Item
    method next(mut self) Option[Self.Item]
}

fn take_one(source Source[Item = Int]) Option[Int] {
    source.next()
}
```

An erased interface binds every associated type declared by the concept,
including associated types that do not appear in a method signature. Missing,
conflicting, or cyclic bindings are compile-time errors.
Associated bindings use brackets and `=`, while the colon remains reserved for
generic bounds.

Concepts may also have `static method` requirements, generic methods, and uses
of `Self` that are valid for static generic dispatch. Those requirements make a
concept ineligible for `dyn concept` when they cannot be erased safely.

## Mutable dispatch

A `mut self` requirement needs a mutable receiver place. Pass a `var` concrete
value or call through a `var` stored `dyn` value:

```loom
var source = erase_source(Counter { value = 0 })
let item = source.next()
```

For a synchronous concrete-to-interface call, the compiler may use a
call-scoped copy-in/copy-out carrier and commit the updated logical value on
normal return. This is compiler-private implementation state, not a reference
or lifetime visible to source code. Async calls copy owned values into their
task frames and do not retain such a caller write-back carrier.

## Dynamic compatibility

Every requirement of a `dyn concept` must:

- be a `self` or `mut self` receiver method;
- have no method-specific type parameters;
- avoid an unbound non-receiver use of `Self`;
- bind associated types used by its callable ABI;
- avoid `static method` requirements;
- use types that have an ordinary runtime representation.

The compiler diagnoses an incompatible declaration at its definition instead
of allowing call sites to disagree.

Type erasure also cannot hide a resource or structured-task obligation. A
concrete value that directly or recursively contains `MustScope` state or an
unconsumed `Task` cannot be adapted into a freely stored `dyn C`. An
unconstrained generic source is rejected conservatively because its contents
are unknown.

## Representation and dead-code elimination

The source language does not specify a fat pointer, object header, vtable slot
order, or stable witness ABI. Depending on reachability and optimization, the
compiler may devirtualize a call, specialize it, pass data and proof separately,
or materialize a compiler-private data/witness representation.

Native reachability starts from the selected executable, test, or exported
library roots and follows actual calls and proof construction. Declaring
`impl C for T` alone does not retain its method bodies. Unused witnesses and
slots can be removed after devirtualization.

Loom deliberately has no universal `any` conversion that can discover
conformance at runtime. In particular, `A -> any -> dyn C` is not a supported
path. There is no reflection registry, class loader, open-world method lookup,
or C++-style vptr embedded in every record.

Physical representation remains an implementation detail and may change while
the language is experimental. Observable value copying, mutation, conformance,
contract, and fault behavior must remain the same.
