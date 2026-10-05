# UDP echo

```sh
target/loom check compiler/examples/udp_echo
target/loom build compiler/examples/udp_echo --output target/udp-echo
target/loom test compiler/examples/udp_echo
target/loom run compiler/examples/udp_echo
```

Two ordinary Tasks exchange a Unicode datagram over real IPv4 loopback sockets.
The client uses a fixed-peer `Connection`; its delayed send lets the server
register a read with the platform reactor.
The source `std.net.udp` library retries readiness, preserves packet boundaries,
returns each packet's numeric sender, and closes sockets through lexical `defer`.
No blocking DNS lookup, polling coroutine or protocol-specific executor is added.

`receive(socket, limit)` returns one fresh `Datagram`; an empty payload is valid
data, not EOF. The default limit is 65535 bytes. Negative or larger limits reject;
an oversized packet is consumed and reports `UdpError.Truncated`, never a silent
partial success. UDP itself promises neither delivery nor ordering. `send` sends
one packet, retains its initial byte extent across retries, and never splits it.

Sockets are owner-local resources. Copies share an identity; finish or cancel
pending I/O before `close`. `abort` revokes aliases and wakes pending I/O, which
must still drain. Use the existing Task deadline helpers for receive deadlines.
`connect(numeric_peer).await` binds an ephemeral local port without a remote
handshake. `send(connection, bytes)` targets only that peer, and the kernel
filters incoming packets from other senders. Connection copies share the same
close/abort identity. The source tests also exercise filtering and empty packets.
IPv6 numeric endpoints are supported when the host enables that family; DNS,
multicast and broadcast configuration remain future work.
