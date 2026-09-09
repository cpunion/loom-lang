# Captured compile-time callbacks

A `comptime` function parameter fixes the call target during checking. Its
capture environment remains ordinary managed data, passed without a
runtime-selected entry pointer:

```loom
fn counter(start Int) fn() Int {
    var value = start
    fn() Int { value = value + 1
        value }
}
fn twice(comptime action fn() Int) (Int, Int) {
    (action(), action())
}
fn main() {
    let first, second = twice(counter(0))
    assert first == 1 && second == 2
}
```

The argument's construction is pure compile-time execution; an explicit
`comptime { ... }` block is optional at this parameter. Its captured graph is
materialized once whenever the source call executes. A separate construction
starts fresh; forwarding the parameter passes the same live environment.
Returning a closure that captures the parameter retains that shared state.
Different captured contents do not create extra native specializations of the
same target and typed environment layout.

Constructing a callback does not execute its body: effectful and async targets
can be saved for later runtime calls. Actually calling a callback at compile
time still validates every discovered possible target of that checked callable
shape, including unexecuted runtime branches. Real I/O and Task creation,
transfer or await cannot run in the evaluator.

An ordinary runtime binding cannot become a static argument. Inside `twice`,
`comptime { action() }` cannot read the live environment; to execute both
construction and invocation at compile time, evaluate the whole call:
`comptime { twice(counter(0)) }`. Required proofs do not assume captured values.

From the repository root:

```sh
target/loom check compiler/examples/comptime_closures
target/loom build compiler/examples/comptime_closures
target/loom test compiler/examples/comptime_closures
target/loom run compiler/examples/comptime_closures
LOOM_GC_STRESS=1 compiler/examples/comptime_closures/target/tests
```

The tests also cover generic forwarding, shared versus independent arguments,
static/dynamic methods, returned closures and asynchronous callbacks.
