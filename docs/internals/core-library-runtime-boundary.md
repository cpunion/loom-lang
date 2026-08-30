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
dead-code elimination as application code. An unused library package must not
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

The core does not own a JSON grammar, collection algorithms, protocol clients,
or convenience I/O composition. Adding a feature to semantic builtin tables
requires evidence that it cannot be expressed as a normal source definition
over smaller primitives.

## Standard library

The standard library is compiler-distributed Loom source. It is loaded as a
read-only, versioned `std` module rather than merged into the user's root
module. Its source is parsed, resolved, type-checked, lowered, instantiated,
and optimized by the ordinary pipeline.

The boundary requires every public library declaration to have an ordinary
source `DefId`. A compiler primitive may be imported only by a package whose
owning module identity is the compiler-owned `std`; application packages and
dependencies must receive an ordinary unresolved-import diagnostic for the same
spelling. Primitive names are compiler-private implementation details, never
public `std` API names. A migrated wrapper records the primitive identity only
inside its reachable source body, so ordinary call-graph closure and dead-code
elimination remain the authority for including library behavior. The
implementation-status document identifies APIs that have not reached this
boundary yet.

The `std.resource` package declares the public `Dispose`, `MustScope`, and
`NoSuspend` concepts in ordinary Loom source. Moving these declarations into
the `std` module removes copies from applications and fixtures; it does not
turn lexical cleanup into a library convention. The compiler identifies the
definitions from its own `std` module, validates their fixed shapes, and
enforces disposal, recursive resource obligations, and suspension restrictions
statically. Concept witnesses lower through the normal direct-call machinery;
there is no source-visible runtime resource registry or name-based runtime
dispatch.

`std.log.LogLevel`, `write`, `debug`, `info`, `warn`, and `error` are ordinary
Loom source declarations. The public `write` wrapper alone calls an exact-owner
private primitive, while each convenience function constructs an empty fields
map and calls the public wrapper through a normal source `DefId`. The complete
public graph therefore participates in ordinary reachability and disappears
when unused. Native compilation accepts logging only through typed LCIR; the
runtime has no universal-value logging entry point.

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

- JSON parsing and the public formatting contract;
- collection and text algorithms built from general construction primitives;
- task-composition conveniences that do not require new scheduler semantics;
- high-level file, socket, logging, and process APIs over narrow platform
  operations;
- protocol, encoding, and data-format packages.

`std.process.arguments` and `std.process.environment` demonstrate the intended
vertical boundary. Both are ordinary source functions with normal `DefId`
calls. `arguments` builds an ordinary `List[Text]` from private count and indexed
selection primitives; `environment` maps the private lookup directly to the
canonical `Option[Text]`. Only their bodies may import those exact primitives.
The driver authenticates compiler-owned source origin together with the exact
`std` module identity; semantic analysis then rechecks that nominal identity
and the owning `std.process` package before accepting either import.
Application imports of the private spelling follow ordinary resolution and
fail; there is no public-name fallback to a builtin. The interpreter owns an
immutable argument snapshot. Native LCIR uses the versioned typed process ABI
and direct Text output cells; process input has no universal-value or
checked-MIR implementation.

`std.io.write` and `std.io.write_line` use the same boundary. Their public
definitions are ordinary Loom source, while only the exact compiler-owned
`std.io` module may import the format-neutral standard-output primitive. The
primitive accepts Text bytes and returns no policy-bearing value; line-feed
composition remains in `write_line` source.

`DecodeTextError` and `PathError` are likewise ordinary public enums declared
by compiler-distributed `std.text` and `std.path` source. Checked MIR retains
their exact source `TypeId` values in its prelude catalog, and typed LCIR
revalidates those identities and variant shapes before decode or path
instructions may construct a result. A same-named application or dependency
enum cannot substitute for either compiler-owned source declaration; there is
no builtin type, builtin variant, or compatibility alias behind the public
names.

The embedded source content is part of compiler-cache identity. Editing an
unused private library body may leave a native object reusable when ordinary
reachability proves that body dead; changing an imported public interface or a
reachable body invalidates the corresponding layers.

## Native runtime

The runtime is compiler-private. Its responsibilities are:

- precise allocation and moving garbage collection;
- typed root registration and compiler-provided object descriptors;
- task scheduling, readiness notification, timers, and platform wait sources;
- narrow operating-system and FFI boundaries;
- generic bulk construction operations where implementing the same operation
  as immutable source-level copies would impose unavoidable quadratic work.

Runtime entry points operate on primitive storage or compiler-provided layout
descriptors. The runtime contains no JSON parser and does not receive a
universal value or source type identifier for parsing. The one format-specific
operation is canonical JSON formatting: generated code supplies the exact
closed `Json` layout, the runtime stages bounded output bytes, and generated
code constructs the checked `Result[Text, JsonError]` value from the returned
status. No runtime registry or dynamic type switch participates.

## Bootstrap primitives

Some source-library algorithms need efficient construction that immutable
public values cannot provide directly. Such primitives remain format-neutral:

- build one `Text` from validated UTF-8 units through the format-neutral
  `Text.from_utf8_units(List[Int])` boundary;
- build one `List[T]` from uniquely owned append state;
- build one canonical `TextMap[V]` from a `List[(Text, V)]` in one bulk
  operation, sorting UTF-8 keys once and rejecting duplicates;
- enumerate a canonical `TextMap[V]` entry by checked index;
- allocate and initialize a compiler-described closed value;
- convert between Unicode scalar values and their canonical encoding.

These are implementation capabilities, not an invitation to expose ownership
or borrowing syntax. The compiler proves uniqueness where an operation needs
it, and the source model remains automatic-memory-managed value semantics.
The source-facing bulk operation is
`List[(Text, V)].to_text_map() Result[TextMap[V], Text]`. Its duplicate
result is the lexicographically smallest duplicated key in canonical UTF-8
order. It is a general collection operation, not a JSON-specific runtime
entry point.

Portable Path operations follow the same narrow-boundary rule. `Path` is one
Text field, so `Path.from_text` validates U+0000 and `Path.as_text` extracts the
field directly without allocating. Only lexical join needs a bulk construction
helper: `loom_runtime_path_join_typed_v1` stages the two complete Text payloads,
rejects a leading `/` in the child, and publishes one Text after a possible
moving collection. Status `0` is success, `-1` is the ordinary `AbsoluteJoin`
outcome, and every other returned status is an ABI defect. It does not inspect
filesystem state, recognize host path syntax, normalize `.` or `..`, collapse
repeated separators, or carry JSON or ownership policy. The checked-MIR native
route has its own whole-value Path operations; typed LCIR does not depend on
them.

## Generated operations

Representation-dependent operations belong in compiler-generated, typed
helpers when they can be derived from a closed type. Recursive equality is the
canonical example: the compiler emits one specialized helper for each reachable
closed type and ordinary direct calls close recursive cycles. The runtime does
not receive a universal value or a type switch, and an unused helper is absent
from the artifact.

## Boundary tests

Every source-backed standard algorithm must prove all of the following:

1. `check`, `build`, `test`, and `run` succeed through both maintained terminal
   backends for the normative fixtures.
2. `LcirOnly` accepts the complete reachable source implementation.
3. Interpreter and native results agree for success, errors, depth limits,
   Unicode behavior, and allocation pressure.
4. Reachability tests show that a program which does not import or call the
   feature contains no feature-specific function, descriptor, symbol, or data
   table.
5. No duplicate builtin, MIR operation, LLVM emission path, runtime ABI, or
   alternate implementation remains beside the source definition.

Versioned artifacts and caches accept only their current exact identities and
fail closed otherwise.
