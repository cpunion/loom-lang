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
The `Header` example combines UTF-8 validity with a constant leading byte.
Workers may store that same byte and prove sampled/entry values without assuming
that all mutable reads are stable or expanding the UTF-8 predicate.
The store can be written directly in a captured worker closure, including a
closure with an explicit `ensures result == 35` contract.

`Utf8` uses the non-copying `std.bytes.is_utf8` predicate. Its admitted reads do
not grant arbitrary writes: an unknown UTF-8 preservation proof still rejects.
The [identity package](identity/main.loom) preserves both UTF-8 and a source
checksum constraint through stores proved to leave the current cell unchanged,
without expanding either predicate. Its pure argument helpers mutate fresh,
unpublished List/Bytes scratch, including a local alias reached through another
container. Existing or possibly published storage needs its own preservation proof:

```sh
target/loom check compiler/examples/byte_constraints/identity
target/loom test compiler/examples/byte_constraints/identity
target/loom run compiler/examples/byte_constraints/identity
```

It is a separate directory package, not part of this package's worker tests.
A read followed by a write is not one atomic operation; a worker build must
independently prove equality with the cell at the store, not a prior read.
The public I/O-free checking library needs a host proof backend for obligations
that exceed its local rules; the CLI uses its compile-time solver. Native
executables do not link that solver or install a storage monitor.
