# Binary file streams

`stream(path)` creates a lazy, scoped `Chunks` producer. The first pull opens
the file; `stream(path, limit)` chooses a positive maximum chunk size instead
of the default 8192 bytes. Open, read and normal close run on the existing
bounded native file workers, not on the scheduler thread.

```loom
import std.file.chunks.stream
import std.file.FileError
import std.stream.Stream
import std.option.Option
import std.result.Result
import std.bytes.length

async fn count(path Text) Result[Int, FileError] {
    scoped source = stream(path, 4096)
    var total = 0
    while true {
        match source.next().await {
            Option.Some(chunk) => {
                total = total + length(chunk?)
            }
            Option.None => {
                return Result.Ok(total)
            }
        }
    }
    Result.Ok(total)
}
```

`Chunks` implements `Stream` with `Item = Result[Bytes, FileError]`. Successful
items are nonempty, fresh buffers of at most `limit` bytes; short reads do not
mean EOF. No UTF-8 validation or framing occurs, and later pulls cannot overwrite
earlier buffers. The producer retains no delivered chunks and never prefetches.
Consumers that collect all chunks still retain the whole file themselves.

EOF closes the file. Open/read/close errors yield one error item, then all later
pulls return `None`. Explicit `close(source).await` is idempotent and reports a
close failure; closing an unpulled producer prevents opening it. `scoped` is the
fallback for early exit, faults and cancellation. Pending operations drain before
fallback close, which cannot suspend; cancelling a running OS call still waits
for completion. GC does not close files. Serialize pulls and close on one cursor.

The standard owned adapters accept fresh factories:

```loom
// In an async function, with std.stream.take imported.
scoped first = take(stream, path, 1)
```

An unpulled zero-item limit does not open the file. Adapters preserve nested
cleanup; directly awaited consumers may borrow the owner until children drain.
See the [byte-counting application](../../../examples/file_chunks/README.md)
and [async stream protocol](../../stream/README.md).
