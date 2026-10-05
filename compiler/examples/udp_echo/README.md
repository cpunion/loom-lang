# UDP echo

```sh
target/loom check compiler/examples/udp_echo
target/loom build compiler/examples/udp_echo --output target/udp-echo
target/loom test compiler/examples/udp_echo
target/loom run compiler/examples/udp_echo
```

Two ordinary Tasks exchange a Unicode datagram over real IPv4 loopback sockets.
The server checks its default-off broadcast permission and explicitly toggles
it without changing its identity. The client uses a fixed-peer `Connection`; its delayed send lets the server
register a read with the platform reactor.
The source `std.net.udp` library retries readiness, preserves packet boundaries,
returns each packet's numeric sender, and closes sockets through lexical `defer`.
The hostname overload uses the existing asynchronous OS resolver; no blocking
lookup on the owner thread, polling coroutine or protocol-specific executor is added.

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
`set_broadcast(socket, enabled)` controls IPv4 broadcast permission and
`broadcast(socket)` queries it; aliases share the option. The source tests
verify configuration, not delivery under arbitrary routing/firewall policy.
`join_multicast(socket, group, interface)` and `leave_multicast` change shared
membership. For IPv4, both arguments are numeric addresses without ports, and
`"0.0.0.0"` selects the OS default interface. For IPv6, the group is an unbracketed
numeric address without a zone, and the interface is an index (0 for OS choice).
Invalid groups, indices, family mismatches and closed sockets return
`UdpError.Membership`; repeated joins/leaves retain OS error semantics, not an
idempotency promise. Closing releases memberships. The example and source tests
check OS membership operations. `set_multicast(socket, MulticastV4 { ... })`
or `MulticastV6` explicitly sets the outbound interface, `loopback`, and `hops`
(0–255: IPv4 TTL or IPv6 hop limit). This does not join a group. Copies share
configuration; it does not retire pending I/O. The OS may reject an unavailable
interface or default-interface request, and failures can leave part of the
configuration applied; close the socket if exact policy is required.
The [multicast exchange](multicast.loom) uses IPv4 TTL zero to send only to local
multicast loopback, with a delayed sender, real readiness and a draining deadline.
This is real packet evidence for the tested hosts, not routed delivery or IPv6
multicast delivery evidence. Host routes and interface policy still apply.
`connect(host, port).await` resolves the host, then selects the first numeric
endpoint accepted by the OS in resolver order. It does not race or probe remote
peers: UDP connect is not reachability evidence. Invalid/failed lookups return
`UdpError.Resolve`; invalid peer ports or exhausted candidates return
`UdpError.Connect`. Use Task deadline helpers for lookup deadlines; an active
native lookup drains before cancellation completes. The numeric one-argument
overload still bypasses DNS, and a numeric-only native program omits the resolver
operation. IPv6 endpoints depend on host support.
