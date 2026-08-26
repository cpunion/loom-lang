# I/O and logging

> Normative for Loom language version 0.3.

File and network operations are asynchronous. `File` and `Socket` are
`MustScope` resources: a successful handle must be bound with `scoped`, and the
end of the innermost block closes it automatically.

## File creation and opening

Two operation families are available. The faulting family represents host I/O
failure as a task fault:

```loom
import standard.file.open_read
import standard.file.create
import standard.file.open_read_path
import standard.file.create_path
```

```text
open_read(Text) Task[File]
create(Text) Task[File]
open_read_path(Path) Task[File]
create_path(Path) Task[File]
```

The recoverable family returns typed I/O errors:

```loom
import standard.file.try_open_read
import standard.file.try_create
import standard.file.try_open_read_path
import standard.file.try_create_path
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

## TCP sockets

```loom
import standard.net.connect
import standard.net.try_connect
```

```text
connect(host Text, port Int) Task[Socket]
try_connect(host Text, port Int) Task[Result[Socket, IoError]]
```

The port must be in the range 0 through 65535. An invalid port faults in
`connect` and returns `IoErrorKind.InvalidInput` from `try_connect`.

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
File close.

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
import standard.log.debug
import standard.log.info
import standard.log.warn
import standard.log.error
import standard.log.write
```

```text
debug(message Text) Unit
info(message Text) Unit
warn(message Text) Unit
error(message Text) Unit
write(level LogLevel, message Text, fields TextMap[Text]) Unit
```

`LogLevel` is the closed value `Debug | Info | Warn | Error`. The four helpers
are equivalent to `write` with their respective level and an empty fields map.

Each call writes one compact UTF-8 JSON line to standard error. Keys appear in
this order:

```json
{"level":"info","message":"started","fields":{"service":"api"}}
```

The level text is lowercase. Messages, field keys, and values use JSON string
escaping. Field keys follow TextMap canonical order. A write failure produces
RuntimeFault code `LogWriteFault`; logging does not disguise a device failure as
a business Result.
