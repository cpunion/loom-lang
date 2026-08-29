# Core, standard library, and runtime boundary

Loom keeps three implementation layers deliberately separate:

```text
language core
  -> defines syntax, static semantics, and irreducible operations

Loom standard library source
  -> defines public protocols, reusable policies, and data-processing algorithms

native runtime
  -> supplies GC, scheduling, platform services, and allocation primitives
```

This boundary is an optimization and maintainability invariant, not only a
source-organization preference. A library feature implemented in ordinary Loom
participates in the same type checking, monomorphization, reachability, and
dead-code elimination as application code. An unused library module must not
force its algorithms or data tables into a native artifact.

## Language core

The core owns behavior that ordinary Loom source cannot express without
circularity or loss of static guarantees:

- lexical and type syntax;
- values, calls, control flow, contracts, concepts, and `dyn` dispatch rules;
- GC-safe managed-value semantics;
- lexical `scoped` and `defer` cleanup, including the static meaning of the
  canonical resource concepts;
- `Task`, postfix `.await`, and coroutine suspension semantics;
- primitive scalar operations and compiler-known layout operations;
- exact checked operations needed to construct and inspect managed values.

The core does not own a JSON grammar, JSON formatting policy, collection
algorithms, protocol clients, or convenience I/O composition. Adding a feature
to semantic builtin tables requires evidence that it cannot be expressed as a
normal source definition over smaller primitives.

## Standard library

The standard library is compiler-distributed Loom source. It is loaded as a
read-only, versioned `std` package rather than merged into the user's root
package. Its source is parsed, resolved, type-checked, lowered, instantiated,
and optimized by the ordinary pipeline.

The `std.resource` module declares the public `Dispose`, `MustScope`, and
`NoSuspend` concepts in ordinary Loom source. Moving these declarations into
the `std` package removes copies from applications and fixtures; it does not
turn lexical cleanup into a library convention. The compiler identifies the
definitions from its own `std` package, validates their fixed shapes, and
enforces disposal, recursive resource obligations, and suspension restrictions
statically. Concept witnesses lower through the normal direct-call machinery;
there is no source-visible runtime resource registry or name-based runtime
dispatch.

The scheduler does maintain a compiler-private ownership ledger on each Task
for built-in File and Socket handles published in typed results. Successful
completed-result consumption moves those entries to the child's non-null owner
Task, which may itself be the root Task, before retiring the child. If
result-take is applied directly to the ownerless root Task, its entries remain
attached to that Task's `owned_result_resources` ledger in the executor-owned
task registry. This internal exactly-once cleanup bookkeeping is neither
source-visible nor ownership syntax, and it does not affect standard-library
reachability or dispatch.

The library owns reusable policy and algorithms, including:

- JSON parsing and formatting;
- collection and text algorithms built from general construction primitives;
- task-composition conveniences that do not require new scheduler semantics;
- high-level file, socket, logging, and process APIs over narrow platform
  operations;
- future protocol, encoding, and data-format modules.

The embedded source content is part of compiler-cache identity. Editing an
unused private library body may leave a native object reusable when ordinary
reachability proves that body dead; changing an imported public interface or a
reachable body invalidates the corresponding layers.

## Native runtime

The runtime is compiler-private and intentionally unaware of standard-library
data formats. Its durable responsibilities are:

- precise allocation and moving garbage collection;
- typed root registration and compiler-provided object descriptors;
- task scheduling, readiness notification, timers, and platform wait sources;
- narrow operating-system and FFI boundaries;
- generic bulk construction operations where implementing the same operation
  as immutable source-level copies would impose unavoidable quadratic work.

Runtime entry points operate on primitive storage or compiler-provided layout
descriptors. They must not switch on `Json` variants, depend on `JsonError`, or
embed a JSON parser/formatter. A data-format-specific runtime ABI is a
transitional implementation and must be removed once its source replacement
passes the migration gates below.

## Bootstrap primitives

Some source-library algorithms need efficient construction that immutable
public values cannot provide directly. Such primitives remain format-neutral:

- build one `Text` from validated UTF-8 units through the format-neutral
  `Text.from_utf8_units(List[Int])` boundary;
- build one `List[T]` from uniquely owned append state;
- enumerate a canonical `TextMap[V]` entry by checked index;
- allocate and initialize a compiler-described closed value;
- convert between Unicode scalar values and their canonical encoding.

These are implementation capabilities, not an invitation to expose ownership
or borrowing syntax. The compiler proves uniqueness where an operation needs
it, and the source model remains automatic-memory-managed value semantics.

Portable Path operations follow the same narrow-boundary rule. `Path` is one
Text field, so `Path.from_text` validates U+0000 and `Path.as_text` extracts the
field directly without allocating. Only lexical join needs a bulk construction
helper: `loom_runtime_path_join_typed_v1` stages the two complete Text payloads,
rejects a leading `/` in the child, and publishes one Text after a possible
moving collection. Status `0` is success, `-1` is the ordinary `AbsoluteJoin`
outcome, and every other returned status is an ABI defect. It does not inspect
filesystem state, recognize host path syntax, normalize `.` or `..`, collapse
repeated separators, or carry JSON or ownership policy. The older
`loom_runtime_path_contains_nul` and
`loom_runtime_path_join` entries are temporary implementation details of the
still-maintained complete legacy emitter, not dependencies of typed LCIR.

## Generated operations

Representation-dependent operations belong in compiler-generated, typed
helpers when they can be derived from a closed type. Recursive equality is the
canonical example: the compiler emits one specialized helper for each reachable
closed type and ordinary direct calls close recursive cycles. The runtime does
not receive a universal value or a type switch, and an unused helper is absent
from the artifact.

## Migration gates

A compiler or runtime special case can be removed only after its Loom source
replacement proves all of the following:

1. `check`, `build`, `test`, and `run` succeed through both maintained terminal
   backends for the normative fixtures.
2. `LcirOnly` accepts the complete reachable source implementation.
3. Interpreter and native results agree for success, errors, depth limits,
   Unicode behavior, and allocation pressure.
4. Reachability tests show that a program which does not import or call the
   feature contains no feature-specific function, descriptor, symbol, or data
   table.
5. The old builtin, MIR operation, LCIR instruction, LLVM emission path,
   runtime ABI, and compatibility identity are deleted together.

There is no permanent compatibility route for superseded internal compiler or
runtime representations. Versioned artifacts and caches fail closed and are
rebuilt after a boundary change.
