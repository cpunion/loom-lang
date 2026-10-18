# Stable shared contents

```sh
target/loom check compiler/examples/constant_contents
target/loom test compiler/examples/constant_contents
target/loom run compiler/examples/constant_contents
```

A parent invariant may observe a constrained List or Bytes child when the
compiler proves that any two valid child values have identical contents and
length. The rule composes scalar, record, tuple and enum representations; it
does not enumerate application types or install a runtime monitor.

Every actual store still needs to preserve the complete child predicate.
Unobserved sibling Lists remain mutable. The example retains the parent across
real suspension and a worker while aliases perform legal identity stores.

Unknown proofs reject. A positive-element constraint does not establish stable
contents, and numerical Float equality alone does not distinguish signed zeros.
Arbitrary mutable nested graphs remain outside this guarantee.
