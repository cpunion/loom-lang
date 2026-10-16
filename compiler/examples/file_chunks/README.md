# Streaming binary byte count

```sh
target/loom check compiler/examples/file_chunks
target/loom test compiler/examples/file_chunks
target/loom run compiler/examples/file_chunks -- README.md
```

The application counts raw bytes with `std.file.chunks` and `std.stream.try_fold`,
without UTF-8 decoding or retaining the whole file. The lazy producer opens on
its first pull. Open/read/normal close use bounded native workers; scope cleanup
closes an unfinished file after pending Tasks drain. The colocated test crosses
the default chunk boundary with invalid UTF-8 bytes.
