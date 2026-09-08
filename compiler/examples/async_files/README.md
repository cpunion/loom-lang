# File tasks

```sh
target/loom check compiler/examples/async_files
target/loom run compiler/examples/async_files -- INPUT OUTPUT
```

This binary-safe copy uses `std.file.tasks.read_bytes` and `write_bytes`, returning
ordinary `Task[Result[...]]` values. `read_text` and `write_text` are also available
in that package. The destination is created or truncated, not atomically replaced.

File read/write operations use a lazily created pool of at most four native
workers per owner. Each operation copies write input once or copies read output
back on the owner thread; workers never retain managed pointers. Completion
notifications wake the existing ready queue. Source loops handle partial I/O,
UTF-8 validation and errors, and source `defer` closes on failure/cancellation.

Cancellation removes queued operations. An already-running OS call must finish
before its buffers are released and the parent scope closes. A stuck OS call
can therefore delay cancellation. This is not parallel execution of Loom code.

Open/close and handle duplication are still synchronous. This first file adapter
does not make potentially blocking opens, pipes, sockets, or all filesystem
operations nonblocking. The synchronous `std.file` package remains available
without a task owner; the compiler does not depend on `std.file.tasks`.
