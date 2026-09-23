# Core programming experience

```sh
target/loom check compiler/examples/core_experience
target/loom build compiler/examples/core_experience
target/loom test compiler/examples/core_experience
target/loom run compiler/examples/core_experience
```

The example combines a checked record constraint, a proved concept contract
called through `dyn Normalizer`, and a hot Task that keeps managed values and
lexical `defer`/`scoped` cleanup across a timer await. Its tests live beside the
program and are excluded from an ordinary build.
