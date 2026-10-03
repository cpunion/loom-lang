# Scoped mutex

`loom run compiler/examples/mutex` protects a complete read/modify/write with
`std.sync.mutex`. Aliases share the mutex; its `scoped` guard releases at the
containing block exit. A guard cannot be copied, discarded or held across await.

`loom test compiler/std/sync/mutex` also exercises early return, fault cleanup
and rejected reentrance. Source Tasks still run cooperatively; this example does
not claim that the general worker executor is implemented.
