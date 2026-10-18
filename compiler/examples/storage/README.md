# Typed storage rows

```sh
target/loom check compiler/examples/storage
target/loom build compiler/examples/storage --output target/storage-example
target/loom test compiler/examples/storage
target/loom run compiler/examples/storage
```

`std.storage.record_codec[T]` derives a prepared row codec for a visible record
using ordinary compile-time field iteration. Field names select stable column
IDs, not physical database names. Explicit `(field_name, column_id)` mappings
support renamed fields; descriptor order determines row order.

The example supplies an explicit `ColumnValue` implementation for constrained
`Amount`, delegating to the source `encode_refined_column` /
`decode_refined_column` helpers. Decoding calls the ordinary checked constructor,
so a negative stored amount is rejected. No handwritten record encoder/decoder
or runtime type discovery is needed. The descriptor and mapping are frozen during preparation;
record values and Bytes retain ordinary sharing.

This example does not access a database or prove a schema upgrade safe. Deployment
planning still needs an explicit migration and recovery policy.
