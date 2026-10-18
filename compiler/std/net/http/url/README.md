# HTTP(S) URLs

`std.net.http.url.parse(text)` is a pure parser returning `Result[Url, ParseError]`.
It has no network I/O, runtime primitive or dependency on the HTTP message codec.
It also works in ordinary `comptime { ... }` blocks.

```loom
let endpoint = parse("https://Example.com:8443/a%2Fb?q=1#local")?
// endpoint.secure    true: choose verified TLS, not a plaintext connection
// endpoint.host      "example.com": DNS/TLS name, without IPv6 brackets
// endpoint.port      8443: connection port
// endpoint.authority "example.com:8443": HTTP Host value
// endpoint.target    "/a%2Fb?q=1": origin-form request target
// endpoint.fragment  "local": local reference, never sent as the target
```

The [TCP example](../../../../examples/http_loopback/README.md) uses these fields
for a real connection and encoded request.

The parser accepts absolute `http` and `https` URIs with ASCII registered names
or bracketed IPv6, including a final IPv4 pair. Host/scheme casing normalizes;
numeric ports are 0–65535, empty ports use the scheme default, leading port zeros
disappear and default ports are omitted from the returned authority.
IPv6 spelling is otherwise retained, not canonicalized into a unique address.
Empty paths become `/`; a query without a path becomes `/?...`.
Path/query/fragment escapes are validated but never decoded, dot segments are
not rewritten, and no fragment enters `target`.

Userinfo, malformed percent escapes, whitespace, control/non-ASCII bytes and
backslashes reject rather than receiving browser-style repairs. Encoded/Unicode
hosts, IDNA, scoped IPv6 zones, IPvFuture, relative references and redirect
resolution are not implemented. Errors contain only an enum, never the URL.
These are a bounded subset of [RFC 3986](https://www.rfc-editor.org/rfc/rfc3986.html)
and the [HTTP(S) schemes](https://www.rfc-editor.org/rfc/rfc9110.html#section-4.2),
not a general URL standard implementation.

Parsing does not establish DNS reachability, TLS trust, allowed destinations or
authorization, and is not an SSRF filter or an origin-equivalence proof.
Callers select verified TLS for `secure`, retain request/field limits, and decide
credentials, redirects and connection policy explicitly. Manually constructing
or updating `Url` does not validate its fields.
