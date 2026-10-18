# TCP chunk stream

`read(connection, limit)` returns a `Chunks` implementing
`std.stream.Stream[Item = Result[Bytes, TcpError]]`. The positive limit bounds
each read, not the total input. Pulls use existing nonblocking TCP readiness;
there is no new runtime ABI.

Each successful item owns a fresh Bytes buffer. TCP may split or combine writes;
chunks are not messages, lines, or independently valid UTF-8. `None` means EOF.
A read error is yielded once as `Some(Err(...))`, followed by `None`.

The stream borrows the socket identity. Copies share its end flag, and callers
serialize pulls. EOF, errors and cancellation do not close the socket: the
caller closes it explicitly or in an enclosing `defer`, after pending pulls
drain. Cancellation retires readiness leases but cannot roll back bytes already
read. A canceled pull does not mark an otherwise open socket ended.

Use `try_fold` to stop on an error without accumulating the whole input;
`collect` buffers all yielded items. Library tests use loopback sockets and
exercise EOF, errors, preserved buffers and cancellation followed by another read.

## Owned connections

`open(numeric_address, limit).await` or `accept(listener, limit).await` returns
`Result[OwnedChunks, TcpError]`. Both require a positive limit. Unlike `read`,
these acquire and retain a [scoped connection](../README.md); no custom socket
resource wrapper is needed:

```loom
async fn incoming(address Text) Result[OwnedChunks, TcpError] {
    open(address, 8192).await
}

// Inside an async function, with the usual stream imports:
scoped source = take(incoming, address, 2).await?
let values = collect(source).await
```

Acquisition performs no read. EOF/errors end the cursor but retain the socket
until scope exit. Pending pulls drain before nested disposal closes it.
Cancellation and unclaimed acquisition results carry the same typed cleanup;
a zero-item limit still connects and closes without reading.
