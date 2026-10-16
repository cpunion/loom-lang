# Constrained binary storage

```sh
target/loom check compiler/examples/byte_constraints
target/loom test compiler/examples/byte_constraints
target/loom run compiler/examples/byte_constraints
```

Bytes use the same checked sequence construction, alias and per-store proof
rules as Lists. The example checks a fresh header once, shares it across a real
wait, updates it through a finite source helper, and copies it explicitly when
unrestricted mutation is wanted. A byte-range fault happens before its store;
the original header still satisfies its invariant after the Task faults.
Colocated tests run concurrent workers over both Bytes and List views. A sampled
element retains its type's continuing predicate, not equality with a later read.

`Utf8` uses the non-copying `std.bytes.is_utf8` predicate. Its admitted reads do
not grant arbitrary writes: an unknown UTF-8 preservation proof still rejects.
The public I/O-free checking library needs a host proof backend for obligations
that exceed its local rules; the CLI uses its compile-time solver. Native
executables do not link that solver or install a storage monitor.
