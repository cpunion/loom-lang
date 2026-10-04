# Shared fixed-shape views

```sh
target/loom check compiler/examples/shared_views
target/loom test compiler/examples/shared_views
target/loom run compiler/examples/shared_views
```

`std.list.view.capture(source, start, end)` captures element identities in a
fixed, half-open range. Omitting the bounds captures the whole current List.
`length`, `get` and `set` operate on the view. Copies share those identities.

The explicit-range overload proves `length(result) == end - start` on normal
return, together with valid scalar bounds. `exact_pair` uses this contract to
construct a length-two refinement directly, without a `Result` or a second
predicate check. `retained_length` composes that guarantee across source edits.
Invalid ranges still fault at the original capture boundary.

Private range metadata carries an ordinary scalar type constraint. `length`
therefore proves a nonnegative result even for an arbitrary `View[T]` parameter,
as `nonnegative_length` demonstrates; callers need no repeated assertion or
construction history. Shared source and retained cells remain unconstrained.

Updates remain visible through the source and overlapping views. When an element
is removed from the source, existing views retain it together; appending creates
a new element, not a replacement in old views. Growth and moving GC do not change
these rules. The example also covers suspension, length refinements, compile-time
execution and bringing a partially detached view graph into runtime.

This is not an isolated snapshot or a read-only List. A view retains its source
List, and removed elements stay alive while a retaining view is alive. Task and
MustScope elements cannot acquire shared ownership through a view. A fixed length
does not establish content constraints such as positivity or sortedness.
