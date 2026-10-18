# Associated factories

```sh
target/loom check compiler/examples/associated_functions
target/loom build compiler/examples/associated_functions
target/loom test compiler/examples/associated_functions
target/loom run compiler/examples/associated_functions
```

`Factory.from_number[Positive]` selects a checked implementation without an
existing receiver value. It can be called directly or retained as an ordinary
function value. Its constrained constructor rejects zero at the normal type
boundary; the concept declaration itself grants no unchecked construction.
The default label needs no runtime `Self` argument.

The same concept retains an ordinary receiver method for `dyn Factory`.
Its table contains only that method, not a constructor for the erased payload.
`AsyncFactory.load[Text]` shows a receiver-free async factory, including a
function value returning a hot Task.
