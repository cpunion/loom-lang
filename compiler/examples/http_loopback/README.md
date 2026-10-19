# HTTP over real TCP

```sh
target/loom check compiler/examples/http_loopback
target/loom test compiler/examples/http_loopback
target/loom run compiler/examples/http_loopback
```

The application opens an ephemeral loopback listener and sends a HEAD request
followed by a chunked POST on the same connection. The server reads both with
the source-written [HTTP message codec](../../std/net/http/README.md), returns
HEAD metadata without a body, and echoes the POST's binary payload and trailers.
Small input chunks split protocol lines and payloads across socket reads.
The POST uses the streaming request writer: two binary chunks and a separate
trailer section are sent without assembling a complete encoded request.
The client obtains its host, port, Host field and first request target from the
pure [HTTP(S) URL parser](../../std/net/http/url/README.md); the local fragment
does not enter the request.
Both responses finish while the connection remains open; EOF is not mistaken
for the boundary of a length-delimited or chunked message.
The client scopes the POST response, consumes at most two body bytes per pull,
and accumulates a count/checksum without collecting the response body. Trailers
become available only after the final chunk is consumed.

The listener is scoped. Both manually managed socket identities close through
lexical defer after the server Task drains. No external service, port number,
HTTP runtime, compiler feature or new dependency is required.
The native integration gate also runs the library and application at O0/O2.
