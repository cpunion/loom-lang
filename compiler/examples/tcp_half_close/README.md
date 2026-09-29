# TCP half-close

```sh
target/loom check compiler/examples/tcp_half_close
target/loom run compiler/examples/tcp_half_close
target/loom test compiler/examples/tcp_half_close
```

The client sends a request, ends only its write side, and keeps reading. The
server waits for request EOF before sending its response and ending its own
write side. The program needs no external server and prints `half-close roundtrip`.

`local_address` accepts a Listener or Stream; `peer_address` accepts a Stream.
Both return numeric address Text, not a DNS name. `local_port` works with both
wrappers. `set_nodelay(stream, enabled)` controls TCP_NODELAY; it does not disable
Loom or application buffering. `shutdown_write` preserves the token and pending
read registrations, so `close_stream` is still required after tasks drain.

The test also checks exact local/peer endpoint correspondence and stale wrapper
errors after close. Native integration runs the example at O0/O2 with moving GC.
