# Memory and resources

> Normative for Loom language version 0.3.

## Automatic memory management

Loom automatically manages ordinary program memory. Source code has no
ownership, move, borrow, lifetime, raw-pointer, or manual-allocation syntax.
Managed values may move in memory, and their address or storage identity is not
observable.

Automatic memory management does not change value equality, constrained-type
predicates, contracts, or concept dispatch. An implementation may share
immutable storage as long as logical value semantics remain unchanged.

Language version 0.3 has no finalizer, weak-reference, destructor declaration,
or stable-address API. Files, sockets, locks, transactions, and other external
resources therefore require lexical cleanup. They must not rely on memory
collection. Allocation failure is a non-recoverable runtime fault, not a
business `Result`.

## Lexical cleanup scopes

Every braced block is a cleanup scope, including a function body, an ordinary
block expression, each `if` or `else` block, a match-arm block, and each loop
iteration body.

Cleanups registered in a block run whenever control leaves that block:

- normal fallthrough or completion of its tail expression;
- an explicit `return` or `?` propagation;
- a contract or runtime fault;
- cancellation of an async task.

They run in reverse registration order. Inner-block cleanup completes before
control continues through an outer block. This is block-scoped behavior, not a
function-wide defer policy.

All registered cleanups continue to run if one cleanup faults. During an
otherwise normal exit, the first cleanup fault encountered in LIFO order
becomes the primary failure. During unwinding from an existing contract fault,
runtime fault, or cancellation, a cleanup fault does not replace that original
outcome. Suppressed cleanup details are not available to source code.

## `scoped`

`scoped` binds a value whose cleanup method is known statically:

```loom
scoped file = acquire_file()
scoped socket Socket = acquire_socket()
```

The optional type follows the name without a colon. Ordinary local type
annotations remain invalid.

The initializer runs before cleanup is registered. If initialization completes
successfully, exactly one cleanup is registered in the current block. The
binding itself is stable: it cannot be reassigned, copied, returned, stored in
another value, passed as an ordinary value, or allowed to escape its block.
Manual invocation of the registered disposal operation is rejected, preventing
double cleanup.

`scoped` does not create a mutable `var` place. User code may call read-only
methods on the binding, but an ordinary `mut self` method still requires `var`
and is therefore rejected. The registered `Dispose.dispose(mut self)` call is
performed only by lexical cleanup.

`File` and `Socket` are built-in scoped resources. A package defining a custom
resource imports the canonical concepts from the compiler-distributed
`std.resource` source package:

```loom
import std.resource.Dispose
import std.resource.MustScope
import std.resource.NoSuspend
```

Applications do not redeclare these concepts. The `std` module declares
`Dispose` as a non-dynamic concept containing exactly
`method dispose(mut self)` without contracts; `MustScope` and `NoSuspend` are
empty non-dynamic marker concepts. Their declarations are ordinary public Loom
source, while their fixed shapes and static meaning are language rules. They
do not create a runtime resource registry.

`scoped` requires a unique `Dispose` conformance. A type that conforms to
`MustScope` must enter `scoped`; ordinary `let`, explicit discard, and implicit
loss at block exit are not alternatives. While a scoped `NoSuspend` value is
active, execution cannot cross `.await`.

## Recursive `MustScope` obligations

The obligation is recursive. A tuple, list, map, option, result, task outcome,
record, or enum that contains a `MustScope` value still carries the obligation.
Wrapping a resource does not make it discardable or freely storable.

A resource produced inside `Option` or `Result` must be unwrapped directly into
`scoped`. Valid forms include:

```loom
scoped file = try_open_read(path).await?

match try_open_read(path).await {
    Ok(file) => {
        scoped file = file
    }
    Err(error) => handle(error)
}
```

After a match payload introduces a resource binding, that binding must be used
immediately as the initializer of `scoped`; it cannot first be copied to an
ordinary alias. A wildcard cannot discard a resource-bearing payload.

Awaiting a task does not make an external resource belong to the garbage
collector. When a completed child delivers a File, Socket, or value containing
one to another task, the runtime transfers the resource to that task before the
child is retired. A completed root keeps it on the root Task in the
executor-owned task registry. A faulted, cancelled, losing, or unconsumed task
delivers no resource; its established cleanup path remains responsible for
exactly-once disposal. These are implementation guarantees behind the same
recursive `MustScope` rule, not ownership or borrowing syntax.

Calling a method on a `MustScope` value requires that the receiver already be a
`scoped` binding. Built-in File and Socket methods follow the same rule.

## `defer`

`defer` registers an arbitrary synchronous cleanup block:

```loom
defer {
    release_by_protocol_name(handle)
}
```

The block is not executed at registration. It reads its referenced bindings
when cleanup runs. A deferred block must have type `Unit` and cannot contain
`return`, `.await`, `?`, another `defer`, a new `scoped` binding, or loop control
that targets a loop outside the cleanup. A loop wholly inside the cleanup may
use its own `break` and `continue`. The cleanup may call ordinary synchronous
functions and methods.

Explicit deferred blocks and automatic scoped disposal share one LIFO order:

```loom
scoped first = acquire_first()
defer { release_second(second) }
scoped third = acquire_third()
```

The exit order is `third.dispose()`, `release_second(second)`, then
`first.dispose()`.

## Discard and static obligations

`discard expression` is valid for an ordinary concrete value. It evaluates the
expression and ignores the result. It cannot erase an obligation:

- a direct or nested `MustScope` value cannot be discarded;
- a direct or nested live `Task` cannot be discarded;
- an unconstrained type parameter, `Self`, or unresolved associated projection
  cannot be discarded when the checker cannot prove it free of both;
- conversion to `dyn C` is rejected if it would hide either obligation.

There is no user-visible `MustUse`, `Discardable`, or move-only type hierarchy.
These checks are limited to lexical resources, structured tasks, and types
whose generic shape makes those obligations uncertain.
