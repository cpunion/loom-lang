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

All sockets close explicitly through lexical `defer`. Framing is ordinary Loom
library code over the existing byte-stream and Task interfaces.
