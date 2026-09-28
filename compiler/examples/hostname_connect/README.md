# Hostname connections

```sh
target/loom check compiler/examples/hostname_connect
target/loom test compiler/examples/hostname_connect
target/loom run compiler/examples/hostname_connect
```

The example opens an IPv4 loopback listener, resolves `localhost`, connects and
transfers a byte. It needs no external network service.

`std.net.dns.resolve(host, port).await` returns `Result[List[Text], ResolveError]`:
numeric IPv4/IPv6 socket addresses in OS resolver order. Supply a separate port
in `0..65535`; numeric IPv6 hosts use `::1`, without brackets or a port. Empty or
NUL-containing hosts and invalid ports return `InvalidInput`; failed or empty
lookups return `Lookup`. Hosts-file entries use the OS resolver too.

`std.net.tcp.connect(host, port).await` tries those addresses sequentially,
closing failed attempts. `TcpError.Resolve` means resolution failed;
`TcpError.Connect` means all connection attempts failed. The single-argument
`connect(address)` and `listen(address)` still require numeric socket addresses.

DNS shares the bounded native I/O worker pool and completion queue with file
Tasks. Workers retain no managed pointers. Cancellation drains a running OS
lookup, so a stalled resolver can delay cancellation. This is not a hard timeout,
DNS cache, parallel address race, or TLS implementation.
