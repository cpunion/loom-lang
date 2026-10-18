# TCP connections and listeners

`listen(numeric_address).await` returns `Result[Listener, TcpError]`. Bind the
result through `scoped`; block exit closes it after pending borrows drain.
An unclaimed completed Task result retains the listener's cleanup too.
`close_listener(listener)` permits idempotent early close. Address, buffer and
accept operations on an inactive owner return `TcpError.Close`.
The private state is itself scoped; reboxing it cannot manufacture another owner.

```loom
scoped listener = listen("127.0.0.1:0").await?
scoped connection = accept_owned(listener).await?
```

Borrowing a scoped listener requires directly awaiting the call. A protocol
that retains parallel accept Tasks can explicitly detach to a copyable
`ListenerHandle`; register manual cleanup before another fallible operation:

```loom
scoped owner = listen("127.0.0.1:0").await?
let listener = detach(owner)?
defer {
    discard close_listener(listener)
}
```

Detachment deactivates the owner without closing the socket. A second detach
fails; later owner disposal does nothing. `ListenerHandle` supports the same
address/buffer operations, `accept` and `accept_owned`. Drain its pending accept
Tasks before `close_listener(handle)`; the handle itself has no automatic cleanup.

`open(numeric_address).await` and `accept_owned(listener).await` return
`Result[Connection, TcpError]`. Extract a connection through `scoped`:

```loom
import std.net.tcp.open
import std.net.tcp.write_bytes
import std.net.tcp.TcpError
import std.text.to_bytes
import std.result.Result

async fn send(address Text) Result[Int, TcpError] {
    scoped connection = open(address).await?
    write_bytes(connection, to_bytes("hello\n")).await
}
```

Connection overloads support `read`, `write_bytes`, `shutdown_write`,
`local_address`, `peer_address`, `set_nodelay` and idempotent `close`.
Scope exit closes after pending borrowed operations drain, including faults
and cancellation. A completed but unclaimed acquisition Task owns the connection
and closes it when its result is abandoned. Cleanup cannot suspend.
`open(host, port)` and `open(host, port, options)` use the same bounded DNS,
address-family racing and total cancellation deadline as `connect`, while
retaining scoped ownership of the winning result.

Acquisition constructs the resource in the socket-acquiring Task itself.
Wrapping an ordinary `connect().await` result in a later resource constructor
does not protect the earlier unclaimed, copyable socket result.

The lower-level `Stream`/`ListenerHandle` APIs are explicit socket identities:
`connect`/`accept` return copyable tokens, not scoped owners. Callers must close
them explicitly, including completed Task results they choose not to use.
They are useful for explicitly managed shared protocols; they are not a transfer
of ownership through a Task.

`detach(connection)?` explicitly transfers responsibility to a copyable `Stream`
without closing the socket. The scoped owner becomes inactive: I/O and a second
detach return `TcpError.Close`; its later `close`/disposal do nothing. Register
manual cleanup before suspending or calling fallible application code:

```loom
let raw = detach(connection)?
defer {
    discard close_stream(raw)
}
```

This is a low-level protocol handoff, not automatic resource erasure. TLS uses it
to retain acquisition cleanup until its own explicitly managed connection is ready.

For bounded reads and scoped stream composition, use
[owned TCP chunks](chunks/README.md), optionally followed by
[line framing](../../stream/lines/README.md) or
[binary framing](../../stream/length32/README.md).
