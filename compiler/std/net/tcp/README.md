# TCP connections

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

Acquisition constructs the resource in the socket-acquiring Task itself.
Wrapping an ordinary `connect().await` result in a later resource constructor
does not protect the earlier unclaimed, copyable socket result.

The lower-level `Stream`/`Listener` APIs remain explicit socket identities:
`connect`/`accept` return copyable tokens, not scoped owners. Callers must close
them explicitly, including completed Task results they choose not to use.
They are useful for explicitly managed shared protocols; they are not a transfer
of ownership through a Task. Listener lifetime is still managed explicitly.
Hostname racing/options currently use this lower-level interface.

For bounded reads and scoped stream composition, use
[owned TCP chunks](chunks/README.md), optionally followed by
[line framing](../../stream/lines/README.md) or
[binary framing](../../stream/length32/README.md).
