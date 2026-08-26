# Resources and cleanup

Loom uses automatic memory management for managed memory and lexical cleanup
for external resources. It does not add ownership, borrowing, lifetime, or
pointer-stability syntax.

The cleanup examples in
[`examples/core03/tasks.loom`](../../examples/core03/tasks.loom) and the typed
I/O cases in
[`fixtures/standard-library/main.loom`](../../fixtures/standard-library/main.loom)
are executable evidence for this guide.

## Memory and resources are different

The native runtime has a precise moving tracing collector. Managed object
addresses and relocation are not observable in Loom source. Moving an object
does not change value equality, record invariants, contracts, concept dispatch,
or task identity.

The collector does not run user cleanup code. Core has no finalizers, GC
destructors, or weak references, and collection timing is not observable.
Files, sockets, locks, transactions, and other external resources therefore
need deterministic lexical cleanup.

Out-of-memory failure is an uncatchable process-level runtime fault. FFI is not
currently a supported language surface; a future FFI boundary must copy values
or explicitly pin managed storage at that boundary rather than expose stable
addresses throughout ordinary source.

## Lexical cleanup scopes

Cleanup belongs to the innermost block, not to the whole function. A plain
brace block, an `if` or `else` block, a `match` arm block, a loop body, and a
function body are all scopes.

Leaving a scope runs registered cleanup in last-in, first-out order on every
exit path: falling through, evaluating a tail expression, returning early,
propagating `Err` with `?`, unwinding a fault, or cancelling a task.

```loom
fn cleanup_order() Int {
    var order = 0
    {
        defer {
            order = order * 10 + 1
        }
        defer {
            order = order * 10 + 2
        }
        Unit
    }
    order // 21
}
```

The next outer statement observes cleanup from the inner block already
completed.

## `scoped` bindings

Bind a resource with `scoped`, not `scoped let`:

```loom
scoped resource = Resource { value = 3 }
```

The binding is stable and cannot be reassigned. In the current language,
`scoped` does not also make the binding a mutable place, so source code cannot
call an ordinary `mut self` method through it. The compiler can still invoke
the statically selected disposal operation exactly once on scope exit.

Built-in `File` and `Socket` values have compiler-known close operations. A
custom resource uses the canonical concepts in module `standard.resource`:

```loom
concept Dispose {
    method dispose(mut self) Unit
}

concept MustScope {}
concept NoSuspend {}
```

`Dispose` selects lexical cleanup. A type implementing `MustScope` must enter a
`scoped` binding instead of being silently stored, passed, or discarded. A
`NoSuspend` resource may be scoped only across code with no `.await` point.
These declarations are compiler-known, non-dynamic concepts with fixed shapes;
they do not create a runtime resource registry.

```loom
impl Dispose for Resource {
    method dispose(mut self) Unit {
        self.value = 0
    }
}

impl MustScope for Resource {}
```

A scoped value cannot be copied, returned from its scope, captured by a task,
placed in a longer-lived aggregate, or manually disposed. Manual disposal would
make the compiler-generated cleanup run twice and is therefore rejected.

The `MustScope` obligation is structural: wrapping a resource in a record,
enum, tuple, list, map, `Option`, `Result`, or `TaskOutcome` does not hide it.
For a fallible resource acquisition, unwrap directly into `scoped`:

```loom
scoped input = try_open_read(path).await?
```

Alternatively, a successful `match` arm can immediately transfer its payload
to a scoped binding. Ordinary `let`, `_`, and `discard` cannot erase the
obligation.

## `defer`

`defer` registers an arbitrary synchronous cleanup block. Use it when the
release operation is not named `dispose`, or when cleanup involves ordinary
state rather than a `MustScope` resource:

```loom
defer {
    release_with_protocol_name(handle)
}
```

Registration does not run the body. At scope exit the body observes the values
of its lexical bindings at that time. `defer` and `scoped` registrations share
one LIFO order.

A cleanup block must return `Unit` and complete synchronously. It cannot:

- use `.await` or `?`;
- return from the enclosing callable;
- register another `defer`;
- create another `scoped` binding;
- let a scoped value escape.

It may call ordinary synchronous functions and methods.

## Typed file and socket I/O

The standard library exposes fallible asynchronous acquisition and operations:

```loom
import standard.file.try_create
import standard.file.try_open_read

async fn round_trip(path Text) Result[Text, IoError] {
    {
        scoped output = try_create(path).await?
        output.try_write_text("typed I/O").await?
        Unit
    }
    scoped input = try_open_read(path).await?
    input.try_read_text().await
}
```

Path-based variants accept `Path`; `standard.net.try_connect(host, port)` opens
a `Socket`. `try_read_text` and `try_write_text` return tasks carrying
`Result[..., IoError]`.

`File` and `Socket` operations have a narrow compiler/runtime rule: an I/O task
takes a snapshot of the required host handle when the method is called. That
task can then be structurally awaited after the original resource block exits,
without capturing the Loom scoped value or allowing handle reuse to retarget
the operation. This rule is specific to the built-in I/O boundary and does not
enable arbitrary scoped-resource capture.

## Faults during cleanup

Cleanup still runs when the original computation faults or is cancelled. If a
normal exit encounters a cleanup fault, the first fault observed in LIFO order
is primary while remaining cleanup continues. If cleanup is already unwinding
an earlier fault or cancellation, a cleanup fault does not replace that primary
failure.

Cleanup is deterministic source behavior, not a garbage-collector finalizer or
an optimization hint.
