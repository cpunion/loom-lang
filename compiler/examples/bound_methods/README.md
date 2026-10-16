# Bound method values

```sh
target/loom check compiler/examples/bound_methods
target/loom build compiler/examples/bound_methods
target/loom test compiler/examples/bound_methods
target/loom run compiler/examples/bound_methods
```

`let action = receiver.method` evaluates the receiver once and retains its value.
Rebinding the original variable does not replace that receiver; shared children
keep their normal sharing. Accessible fields take precedence in value selection.
Overloads and generic/variadic methods need a uniquely determined fixed function
signature, from explicit type arguments or context. `dyn` receivers retain their
declared interface. Binding an async method schedules nothing; invoking it
enqueues an ordinary hot Task without running the body inline.

The example also covers generic forwarding, compile-time callbacks and returned
compile-time environments. Bound receivers obey the same resource/capture rules
as closures. No alternate runtime or ownership syntax is introduced.
