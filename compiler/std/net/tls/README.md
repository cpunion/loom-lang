# TLS streams and scoped connections

`std.net.tls` implements client/server TLS policy in Loom over the optional
Rustls packet engine. See the [API and trust rules](../../../README.md#source-tasks)
and [real TCP example](../../../examples/tls_loopback/README.md).

Prefer `scoped connection = open(host, port).await?`, or the explicit client
options/numeric-endpoint overloads. Servers use `accept_owned(listener, options)`.
The returned `Connection` implements MustScope; Result/Task outcomes keep its
cleanup, including successful results never claimed by their caller.
Reads, writes, half-close, addresses, ALPN and verified leaf queries accept it.

Scope exit closes the transport after borrowed operations drain. Graceful TLS
close-notify requires an explicit `shutdown_write(connection).await`; cleanup
does not suspend. Explicit `close(connection)` is idempotent.

For protocols retaining concurrent read/write Tasks, `detach(connection)` returns
a manually managed `Stream` and deactivates its scoped owner. `connect`/`accept`
also return Streams directly. Stream aliases share one TLS connection; drain
child Tasks and close it explicitly, normally through `defer`.
This is an explicit lifetime choice, not ownership/borrow syntax or a GC finalizer.
