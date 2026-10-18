# TCP UTF-8 lines

```sh
target/loom check compiler/examples/tcp_lines
target/loom build compiler/examples/tcp_lines
target/loom test compiler/examples/tcp_lines
target/loom run compiler/examples/tcp_lines
```

The loopback sender writes individual bytes while the receiver requests at most
two bytes per chunk. `std.stream.lines` yields LF/CRLF-terminated Text before
connection EOF, preserving split UTF-8 sequences. TCP write shutdown releases the
final unterminated line. A cancelled reader stays exhausted; a new adapter can
use the still-open transport after the cancelled socket wait drains.

The first trial closes sockets through lexical `defer`. `owned.loom` shows the
owned alternative: an async connection factory composes with `lines` and `take`,
and a scoped consumer receives two lines before closing the connection exactly
once. It uses `std.net.tcp.chunks.open`; the wrapper only records test observations.
The sender observes closure, and a deliberately invalid UTF-8 suffix is not
decoded. Framing and ownership composition are ordinary Loom library code over
existing byte-stream and Task interfaces.
