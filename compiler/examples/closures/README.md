# Closures

Anonymous functions use ordinary named, typed parameters and an optional result:

```loom
fn counter(start Int) fn(Int) Int {
    var current = start
    fn(step Int) Int {
        current = current + step
        current
    }
}
```

Only referenced enclosing bindings are captured. A `let` captures its value;
shared Lists and other shared data keep their normal semantics. A `var` captures
one shared binding: later assignments from either scope are visible to every
closure. Returning or copying a closure keeps that binding alive through GC.
A variable declared inside a loop has a new binding on each iteration; a
variable declared outside remains shared across iterations.

`async fn(...)` literals use the same Task and await rules as named functions.
In a control-flow header, parenthesize an immediately called zero-argument
literal (`if (fn() Bool { true })() { ... }`); an unparenthesized `fn() Int`
remains a function type, including in `comptime if T == fn() Int { ... }`.
Capturing a scoped, MustScope, NoSuspend or live Task value is rejected. A closure
can instead accept Task arguments or create Tasks when called. Merely having an
unused resource or Task in the enclosing scope does not capture it.

Pure closures execute inside `comptime` blocks. Returned environments preserve
sharing, while separate evaluations create independent mutable state. Runtime
bindings cannot enter compile-time execution. Captured closures also work as
[`comptime` function parameters](../comptime_closures/README.md): their target is
static and their environment is passed as ordinary managed data. Anonymous
functions with their own generic or compile-time parameters remain unsupported.

From the repository root:

```sh
target/loom check compiler/examples/closures
target/loom test compiler/examples/closures
target/loom run compiler/examples/closures
LOOM_GC_STRESS=1 compiler/examples/closures/target/tests
```

The tests cover shared reassignment, nested escaping captures, compile-time
results, async factories, per-iteration bindings, and block cleanup.
