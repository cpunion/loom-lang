# Typed SQLite rows

Use a fresh disposable database path and an explicit SQLite CLI executable:

```sh
trial_dir="$(mktemp -d)"
loom run tools/deployment/sqlite_rows_trial -- /usr/bin/sqlite3 "$trial_dir/rows.sqlite"
```

`deployment.read_sqlite_rows[T](tool, database, codec)` reads the physical columns
of a frozen `std.storage.PreparedRowCodec[T]` and returns
`Result[List[T], SqliteRowError]`. The trial derives a codec for an ordinary
record, including an explicit constrained-amount decoder, optional text, Bool,
Bytes and Float. It exercises reordered columns, stable-ID aliases, quoted
identifiers, empty/binary/multiline values, integer limits and a subnormal Float.
Invalid constraints, kinds, NULLs and UTF-8 are rejected at the typed boundary.

The trusted CLI runs one read-only SELECT, with literal arguments and quoted
identifiers; user startup commands are disabled. Cells use tagged hexadecimal
payloads, including SQLite CLI's
[`ieee754_to_blob`](https://sqlite.org/floatingpoint.html) binary64 representation.
The CLI must provide that function. No database/JSON support is added to the
language runtime. Errors contain only categories, zero-based row/column indices
and codec errors, not stored values, SQL or stderr.

All rows are loaded into memory, without an ordering promise. Codec callbacks
retain their ordinary effects. Successful reads are not schema compatibility
evidence, proof of application writer behavior, or authorization to migrate;
general migration execution and recovery remain separate work.
