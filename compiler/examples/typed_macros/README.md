# Typed expression macros

```sh
target/loom check compiler/examples/typed_macros
target/loom test compiler/examples/typed_macros
target/loom run compiler/examples/typed_macros
```

`generator!(arguments...)` runs an ordinary pure
`fn(List[std.reflect.Schema]) Text` or a function with the same parameters and
`std.loom.ast.Node` result at compile time. Each descriptor describes
one inferred input type. The result is parsed as one expression and checked in
the generator's package. The generator may live beside its consumers; no
separate macro package or host-language plugin is needed.

`$0`, `$1`, ... refer to input values in generated source. Inputs evaluate once,
left to right, including unused inputs. Repeated placeholders reuse the saved
value; they do not duplicate effects. Positional inputs cannot be shadowed by a
source declaration. Other names resolve in the generator's package, not the
caller's locals. Generated locals and closures retain normal lexical scope.
Execution permissions belong to the caller: generated `.await` needs an async
caller and still checks its live `NoSuspend` bindings. A newly acquired resource
must enter `scoped` before invocation; existing scoped inputs remain borrowed.

The example covers inferred type selection, heterogeneous inputs, nested macros,
private definition-site helpers, opaque foreign types, mutable closure captures,
cleanup, constraints, suspension and required proofs. Generator code and reflection descriptors disappear
from runtime reachability unless separately called or exported as ordinary code.

Generators use the bounded compile-time evaluator and tracked build inputs.
Effects, faults, invalid syntax/types and expansion cycles reject. Abstract
generic inputs need a contextual result type until instantiation; they provide
no proof evidence. Expansion cannot return or propagate `?` across its boundary;
return a Result and apply `?` at the call site instead. Tasks and scoped resources
retain ordinary one-shot and escape checks, not a macro exemption.

The [structured example](trees.loom) constructs and transforms public AST nodes.
AST output must round-trip through the expression grammar before ordinary
checking; malformed trees and internal compiler markers reject. It has the
same hygiene and argument rules as Text output. Top-level declaration macros
remain separate work; [compile-time type values](../type_values) use a distinct
checked path. The
[source generation tool](../ast_generation) demonstrates explicit AST-based
declaration generation without a special compiler path.
