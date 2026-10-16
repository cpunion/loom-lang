# Captured entry receivers

```sh
target/loom check compiler/examples/closures/cells/entries
target/loom test compiler/examples/closures/cells/entries
target/loom run compiler/examples/closures/cells/entries
```

Contracts can observe entry lengths and elements through captured List handles,
nested List indices or inline records containing those handles. For a List, `old(current)` retains a
logical outer version; it does not copy or freeze the mutable object. Rebinding a
capture cannot change the saved observation, and a later invocation takes a new
entry state. Native execution and compile-time execution share these rules.

Intermediate handles supply no deep immutability, non-aliasing promise or renewed
post-call heap constraints. Unknown overlapping writes and shared interference
remain subject to the ordinary storage rules.
