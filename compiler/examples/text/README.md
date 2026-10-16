# Source Text operations

```sh
target/loom check compiler/examples/text
target/loom test compiler/examples/text
target/loom run compiler/examples/text
```

`std.text.replace` replaces exact UTF-8 matches without overlap or normalization.
An empty pattern inserts at every Unicode scalar boundary, including both ends.
The Loom implementation works at runtime and in `comptime`, without a new
runtime primitive. This program prints `Hello, Loom!`.
