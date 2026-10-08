# File lines

`read(path)` opens a synchronous UTF-8 line iterator. Its `Lines` result must
enter `scoped`; early return, block exit and faults close the file. Explicit
`close(reader)` reports close failures and is idempotent. GC never closes files.

```loom
import std.file.lines.read
import std.file.lines.next
import std.file.FileError
import std.result.Result
import std.option.Option

fn count(path Text) Result[Int, FileError] {
    scoped reader = read(path)?
    var total = 0
    while true {
        match next(reader) {
            Option.Some(line) => {
                discard line?
                total = total + 1
            }
            Option.None => {
                return Result.Ok(total)
            }
        }
    }
    Result.Ok(total)
}
```

Each pull returns `Option[Result[Text, FileError]]`: `None` is EOF;
`Some(Err(...))` reports a read, UTF-8 or close failure once, then the iterator
stays exhausted. A close failure after an unterminated final line follows that
line as a separate error item. Invalid UTF-8 is rejected, not replaced.

LF terminates a line; one preceding CR is stripped. Empty lines and a final
unterminated line are retained, but a trailing LF adds no extra empty item.
Bare CR, BOM and NUL are ordinary content. Text values are isolated copies.
The reader buffers one 8 KiB chunk and the current line, not the whole file;
there is no fixed line-length limit.

`Lines` implements [`Iterator`](../../iter/README.md) with
`Item = Result[Text, FileError]`. Import `std.iter.Iterator` for `reader.next()`.
Direct generic consumers such as `collect`, `fold`, `try_fold`, `any` and `all`
may borrow the scoped reader; `collect` retains all yielded values, including
errors. `try_fold` lets a callback propagate each line's Result immediately;
the [application](../../../examples/file_lines/README.md) uses it to count lines
without retaining their contents.
Returning a map/filter/take adapter around that existing borrow is not supported.
The factory overloads instead create a new owner, such as `take(read, path, 2)`;
enter its Result payload through `scoped`. See the
[owned pipeline example](../../../examples/file_pipeline/README.md).

## Async lines

`stream(path)` creates an `AsyncLines` owner without I/O. Its first pull opens
the file asynchronously; `next(reader).await` and `close(reader).await` use the
existing bounded native file workers, not blocking reads on the scheduler thread.
Enter the owner through `scoped`. Open failures are error items on the first pull;
the line framing, UTF-8 checks, EOF and later error behavior match `Lines`.

`AsyncLines` implements [`Stream`](../../stream/README.md), so directly awaited
consumers borrow it until child operations drain. Infallible factory overloads
compose lazy producers without an artificial Result:

```loom
scoped source = take(stream, path, 2)
let lines = collect(source).await
```

The snippet uses `std.stream.take` and `std.stream.collect` in an async function.
An unpulled producer never opens the file, including a zero-item limit. Normal
EOF and explicit close await worker completion. Early exit, errors, cancellation
and faults use synchronous fallback close after pending child operations drain;
cleanup itself cannot suspend. Cancellation of a running OS call still waits
for completion. This does not add general async acquisition returning MustScope
values or borrowing Tasks that outlive their call expression.
See the [async pipeline](../../../examples/stream_lines/README.md).

See the [line-counting application](../../../examples/file_lines/README.md).
