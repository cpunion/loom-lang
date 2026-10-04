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
Returning a resource-containing map/filter/take adapter around that borrow is
not supported. This is synchronous file iteration, not an async stream protocol.

See the [line-counting application](../../../examples/file_lines/README.md).
