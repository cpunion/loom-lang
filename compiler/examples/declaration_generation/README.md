# In-compilation declaration generation

```sh
target/loom check compiler/examples/declaration_generation
target/loom test compiler/examples/declaration_generation
target/loom run compiler/examples/declaration_generation
```

Top-level `comptime { ... }` blocks produce declarations before ordinary binding
and checking. `pair()` returns a public AST record; other blocks return source
Text. Generated functions, refinements, concept implementations and tests obey
the same rules as handwritten code. The imported `api.answer` is also generated.

Generation is one phase over the original source: a generator cannot read another
block's output. Compose ordinary pure functions to combine generators. Output
cannot add imports or another declaration stage; declare dependencies in source.
Names and visibility are explicit in the generated declarations, unlike hygienic
expression macros. No source files are written.

`generated_test.loom` is excluded from production. The generated required
postcondition is proved, and closures retain distinct captures even though editor
diagnostics and definition navigation point to their source generation block.
