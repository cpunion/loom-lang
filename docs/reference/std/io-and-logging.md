# I/O and logging

> Normative for Loom language version 0.3.

File and network operations are asynchronous. `File` and `Socket` are
`MustScope` resources: a successful handle must be bound with `scoped`, and the
end of the innermost block closes it automatically.

## Standard output

```loom
import std.io.write
import std.io.write_line
```

```text
write(Text)
write_line(Text)
```

`write` synchronously writes the exact UTF-8 encoding of its argument to the
process standard-output stream. `write_line` performs the same operation after
appending one line-feed (`U+000A`); it does not substitute a platform-specific
line ending. Neither function performs formatting or adds spaces. A write
failure produces RuntimeFault code `StdoutWriteFault`.

## File creation and opening

Two operation families are available. The faulting family represents host I/O
failure as a task fault:

```loom
import std.file.open_read
import std.file.create
import std.file.open_read_path
import std.file.create_path
```

```text
open_read(Text) Task[File]
create(Text) Task[File]
open_read_path(Path) Task[File]
create_path(Path) Task[File]
```

The recoverable family returns typed I/O errors:

```loom
import std.file.try_open_read
import std.file.try_create
import std.file.try_open_read_path
import std.file.try_create_path
```

```text
try_open_read(Text) Task[Result[File, IoError]]
try_create(Text) Task[Result[File, IoError]]
try_open_read_path(Path) Task[Result[File, IoError]]
try_create_path(Path) Task[Result[File, IoError]]
```

`open_read` opens an existing file for reading. `create` creates or truncates a
file for writing. Text path arguments use the host file-system boundary;
Path-valued variants use the same stored lexical spelling.

The usual recoverable form is:

```loom
scoped input = try_open_read_path(path).await?
```

Cleanup is registered only after the Result has produced a File.

## File operations

On an already scoped File:

```text
file.read_text() Task[Text]
file.write_text(Text) Task[Unit]
file.try_read_text() Task[Result[Text, IoError]]
file.try_write_text(Text) Task[Result[Unit, IoError]]
```

Reading consumes from the file's current position through end of file and
requires valid UTF-8. Recoverable invalid text is `IoErrorKind.InvalidInput`.
Writing attempts to write the complete UTF-8 encoding of the Text from the
current position.

The non-`try_` methods turn host rejection into task-local RuntimeFault values,
such as `FileReadFault` or `FileWriteFault`. The `try_` methods return
`Err(IoError)` for the corresponding expected failure.

File cleanup is lexical. A manual `file.close()` call on the required scoped
receiver is rejected as `ManualDisposeOfScopedValue`; there is no valid
double-close or function-exit-only cleanup pattern.
The lexical destructor performs a final RAII close. It does not expose or retry
the host close syscall's completion status, because a reported error can occur
after the numeric handle has already been released. The current Core slice does
not promise durable storage; a future durability guarantee requires an explicit
flush/sync API rather than destructor status.

## TCP sockets

```loom
import std.net.connect
import std.net.try_connect
```

```text
connect(host Text, port Int) Task[Socket]
try_connect(host Text, port Int) Task[Result[Socket, IoError]]
```

The port must be in the range 0 through 65535. An invalid port faults in
`connect` and returns `IoErrorKind.InvalidInput` from `try_connect`.
Host resolution tries every returned address in resolver order and accepts the
first successful connection. An empty result is a resolution failure; if every
address rejects the connection, the final host error determines the I/O error.

On an already scoped Socket:

```text
socket.read_text() Task[Text]
socket.write_text(Text) Task[Unit]
socket.try_read_text() Task[Result[Text, IoError]]
socket.try_write_text(Text) Task[Result[Unit, IoError]]
```

`write_text` writes the complete UTF-8 encoding. `read_text` reads until the
peer reaches end of stream and then validates the complete response as UTF-8.
Recoverable invalid UTF-8 maps to `InvalidInput`.

Socket methods create structured child tasks and preserve cancellation and
cleanup behavior. Manual `socket.close()` is rejected for the same reason as
File close. Socket destruction follows the same final, non-retryable RAII close
rule.

## `IoError`

`IoError` is opaque source data with two accessors:

```text
error.kind() IoErrorKind
error.message() Text
```

`IoErrorKind` is a closed value:

```text
NotFound
PermissionDenied
AlreadyExists
InvalidInput
ConnectionRefused
ConnectionReset
TimedOut
UnexpectedEof
Closed
Other
```

The kind is stable for matching. `message` is human-readable detail and is not
a stable comparison key. Host error integers are not exposed. Runtime defects,
allocation failure, and compiler/runtime incompatibility are not represented as
IoError.

## Logging

```loom
import std.log.debug
import std.log.info
import std.log.warn
import std.log.error
import std.log.write
```

```text
debug(message Text)
info(message Text)
warn(message Text)
error(message Text)
write(level LogLevel, message Text, fields TextMap[Text])
```

`LogLevel` is the closed value `Debug | Info | Warn | Error`. The four helpers
are ordinary Loom source functions equivalent to `write` with their respective
level and an empty fields map. Only the public `write` function is backed by
the compiler-private output operation; an unused helper is removed by normal
reachability.

Each call writes one compact UTF-8 JSON line to standard error. Keys appear in
this order:

```json
{"level":"info","message":"started","fields":{"service":"api"}}
```

The level text is lowercase. Messages, field keys, and values use JSON string
escaping. Field keys follow TextMap canonical order. A write failure produces
RuntimeFault code `LogWriteFault`; logging does not disguise a device failure as
a business Result.
