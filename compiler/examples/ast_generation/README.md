# Generate a package with Loom

This source-written tool constructs record declarations from field metadata,
combines them with parsed functions and tests, and emits formatted Loom source.
The generated function has an ordinary required postcondition.

```sh
target/loom build compiler/examples/ast_generation --output target/ast-generator
mkdir -p target/generated-example
target/ast-generator > target/generated-example/main.loom
target/loom check target/generated-example
target/loom test target/generated-example
target/loom run target/generated-example
```

The integration test executes this complete pipeline. Generated declarations
are ordinary source files, so the normal package, visibility, contract, test
isolation and build rules apply. This explicit generation step is not an
in-compilation declaration macro or a first-class compile-time type value.
