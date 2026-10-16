# Framed TCP messages

This native loopback example uses the source-written
[`std.stream.length32`](../../std/stream/length32/README.md) adapter over TCP
chunks. It cancels an initial pending read, then receives text, binary and empty
messages across split headers. The sender keeps its write side open until all
three frames arrive, so frame completion cannot depend on EOF.

Sockets close explicitly after pending tasks drain; no framing runtime or
executor extension is involved.

```sh
target/loom check compiler/examples/framed_tcp
target/loom run compiler/examples/framed_tcp
target/loom test compiler/examples/framed_tcp
```
