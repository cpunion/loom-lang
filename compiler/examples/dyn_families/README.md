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
family whose inner interface captures the outer template parameter. Runtime
dispatch still uses only reachable concrete method instantiations. It performs
no runtime type discovery and the type template has no native value layout.
