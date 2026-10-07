# Async stream pipeline

```sh
target/loom check compiler/examples/streams
target/loom build compiler/examples/streams
target/loom test compiler/examples/streams
target/loom run compiler/examples/streams
```

The source `std.stream` pipeline lifts a shared range cursor, asynchronously maps
three items through a dynamic Stream and stops without pulling the suffix.
Managed Text values survive real timer suspension. There is no new language
syntax or stream-specific native machinery. Tests are excluded from library builds.
