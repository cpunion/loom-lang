# Dynamic associated families

```sh
loom check compiler/examples/dyn_families
loom run compiler/examples/dyn_families
loom test compiler/examples/dyn_families
```

`dyn Lists[Item[T] = List[T]]` binds the whole type template, not just the
particular argument types used by this program. The concrete implementation
must match for every argument admitted by the concept. Parameter spelling does
not change that binding; an outer captured type does.

The example combines List results, a captured record parameter and a nested
family whose inner interface captures the outer template parameter. A bounded
projection template normalizes `T.Data` when its generic method is called. Runtime
dispatch still uses only reachable concrete method instantiations. It performs
no runtime type discovery and the type template has no native value layout.

[Computed templates](computed.loom) use pure helpers and compile-time function
values to construct the same structural bindings. The example covers signatures,
lexical shadowing, captured outer types, nested templates and known selected
branches. Computation helpers and unselected branches do not become native roots.
Unknown type-dependent decisions remain errors rather than sampled specializations.
