# Language foundation

Status: **Accepted direction (2026-09-06)**.

This records requirements for Loom's language foundation, not implemented
behavior or a versioned language reference. Code marked **design synopsis**
illustrates intended semantics; helper names and unfinished syntax are not
claims about programs accepted by today's compiler.

## Purpose

Loom should support ordinary applications and its own compiler with a small,
statically checked language. Source, types, contracts, concepts, compilation,
libraries, and tools form one usable system. Implementation stages must not
silently reduce these requirements to the capabilities of a prototype.

## Source and packages

A directory is a package; no `src/` directory is required. Ordinary text files
remain the editing and distribution surface. `pub` controls access across
package boundaries; files within a package share private declarations.

Tests live beside production files as `*_test.loom`. Ordinary `.loom` files
may also contain `test fn` declarations. Test compilation can access the
package's private production declarations. Production compilation excludes test
declarations and test files; production code cannot depend on test helpers.
Package startup is explicit, not an implicit initialization hook. Compile-time
constants do not introduce mutable package initialization.

Modules may resolve multiple dependency versions, preferring compatible
unification. A locked build preserves its dependency graph. Fork selection is
local by default; graph-wide selection is explicit in a dependency entry,
without a separate `replace` table.

## Compiler libraries and tooling

Loom programs must be able to use the language's parser and analysis as
libraries, not only through the compiler executable. Keep these boundaries
separate:

- **Syntax:** parse supplied source text into structured syntax with source
  spans and diagnostics. This lightweight layer does not discover projects,
  read files, resolve imports, invoke the CLI, or load a native backend.
- **Projects:** explicitly load files, manifests, and selected package/test
  dependencies. In-memory parsing remains usable without filesystem access.
- **Semantics:** optionally request resolved declarations, bindings, inferred
  types, and diagnostics for a selected program basis. Syntax tools need not
  type-check a complete project. Semantic results identify their basis so
  clients cannot mistake stale results for current facts.
- **Metaprogramming:** typed reflection and code generation reuse the syntax
  and semantic infrastructure, subject to visibility, staging, and tracked
  build-input rules. They do not bypass those rules through a tooling API.

Ordinary files remain the user's editing surface. Source spans describe a
particular revision's locations; they are not persistent declaration identities.
Semantic-change tools combine these libraries with the separate identity and
history metadata needed to distinguish a move from an edit, as specified in
[Changes and deployment](change-and-deployment.md#story-2-merge-ordinary-source-and-retain-useful-feedback).
Library access must not require adopting a source database or compiler driver.
These are responsibility boundaries, not settled package names, a stable AST
schema, or a promise that every internal compiler structure becomes public API.

## Functions and polymorphism

Parameters and fields use `name Type`. An omitted return type means no value
result, called `Unit` internally; `Unit` is not a source type or expression.
A matching tail expression returns its value; a final `return` is unnecessary.
A no-result function needs neither an explicit tail nor a final `return`.
This does not silently discard a value-producing expression in that function.

Overloads may differ by parameter type or arity. A call must have a determined
selection; unresolved ambiguity requires explicit selection by the programmer.
Overloading does not replace generics or variadic parameters.

Public generic requirements are explicit: an unconditional `value.display()`
requires a declared `Display` requirement. Compile-time conditional branches
introduce only their own local
requirements; an unselected branch imposes none on that instance. The following
function does not require its argument type to implement `Display`.
**Design synopsis:**

```loom
pub fn label[T](value T) Text {
    comptime if T implements Display {
        value.display()
    } else {
        "value"
    }
}
```

Concepts are nominal, with explicit `impl` conformances. Static polymorphism
and `dyn C` use statically established conformance evidence. No runtime
`any`-to-concept discovery or open implementation registry supplies missing
evidence. Concrete representation remains a compiler choice, subject to the
language's observable behavior and dead-code elimination.

Associated types may form type families with their own parameters. A projection
supplies those parameters independently of the receiver's generic arguments;
concept qualification resolves an ambiguous member name. **Design synopsis:**

```loom
concept Family { type Item[T] }
impl Family for Bool { type Item[T] = List[T] }
fn identity[S Family, T](source S, value S.Item[T]) S.Family.Item[T] { value }
```

Member parameters can declare requirements, and the resulting type can have
bounds and a default, as in `type Item[T Display] Display = T`. Implementations
inherit parameter requirements, cannot strengthen them, and must establish the
declared result bounds for every admitted argument, not just observed instances.

## Sharing and persistent constraints

Ordinary data assignment shares mutable data; rebinding one variable does not
rebind another. **Design synopsis:**

```loom
var a = [1, 2]
var b = a
b[0] = 9       // a observes the element update
b = [3]        // a still refers to the earlier container
```

Constraint construction preserves sharing by default, not automatic copying.
This supersedes the earlier isolation-by-default candidate. Construction may
check predicate truth at runtime, but that alone establishes only the current
fact: alias guarantees must also ensure the constraint continues to hold.

Analysis combines the predicate's observed state, operation effects and
contracts, and possible alias writes. An operation may proceed if it does not
affect that state or is guaranteed to preserve the predicate. A constraint
need not freeze unrelated fields or every mutation of a container.
The guarantee continues across calls and suspension; unknown alias activity
is not evidence of safety.

A fixed-shape view can own immutable range metadata while sharing elements.
Its length can remain fixed when the source List resizes; that does not freeze
the original List. Storage relocation must preserve the view's specified
lifetime and sharing. Nonemptiness differs from fixed length: appending can
preserve nonemptiness without preserving length. Content constraints, such as
positivity or sortedness, still require protection against other aliases.
A read-only handle alone provides no such protection.

Unsafe shared strengthening is rejected. Creating a constrained value must not
silently install dynamic monitoring that makes old aliases acquire new runtime
failures. Explicit isolation or an invariant-preserving API can establish a
different, specified boundary. Widening may preserve sharing when the resulting
operations and aliases cannot invalidate continuing guarantees.

Each constructor or view API fixes its observable sharing relationship.
For example, a permitted element update may remain visible through both
aliases even when it preserves nonemptiness. The optimizer cannot choose
sharing versus isolation according to proof difficulty; physical copying or
sharing optimizations must preserve the specified relationship.

## Contracts and proof

Use function `requires`, `ensures`, `result`, and `old(expr)` rather than a
separate property language or verification annotation. `old` denotes the
entry-state logical value observed by its expression, not another mutable
alias. **Design synopsis; predicates and algorithm are omitted:**

```loom
fn sorted_values(xs List[Int]) List[Int]
    ensures sorted(result)
    ensures permutation(result, old(xs))
{
    // sorting algorithm
}
```

Every declared `ensures` is a required static proof under the function's
preconditions. A false, unknown, or timed-out obligation blocks the build.
The declaration itself, or its inserted runtime check, is not its proof.
Ordinary functions do not thereby require complete functional verification.

Proved construction yields the constrained value directly; an unresolved value
predicate retains the boundary's checked failure path. Safe weakening may be
implicit and omit redundant checks, subject to the continuing alias guarantees.

Preconditions, construction checks, and implicit invariant checks can remain
runtime boundary checks. Updates preserve invariants on normal and error
exits. Temporary invalid state must not become observable through aliases,
callbacks, calls exposing that state, or suspension.

Each independently observable update is a boundary. A method can update several
fields within one unobservable boundary; constructing a new valid value is often
simpler. This promises neither automatic rollback on error nor permission to
turn an invalid-then-valid sequence into success by merging checks.

Postconditions concern normal returns only. They do not implicitly prove
termination, absence of faults, or successful external operations. The example
does not promise an unchanged input, a fresh result, or non-aliasing. In-place
sorting instead states its result properties over the updated argument and
compares its elements with the argument's entry-state value.

## Compile-time programming and effects

Constraints can call pure functions but cannot depend on external mutable state.
Purity is inferred; an explicit purity promise is checked. Local mutation and
fresh allocation can be pure when they have no externally observable effect.
Ordinary pure functions can execute at compile time, including conditionals,
loops, and recursion. Templates support type, value, function, and
compile-time-known captured-closure parameters, plus variadic forms. Instances
need only instantiate selected compile-time branches; all branches still parse.
Heterogeneous parameter packs are a language facility, not just a macro trick;
runtime-sized Lists remain distinct.

Macros support typed reflection and access to inferred types. They can live in
the same package as their consumers; no special procedural-macro crate is
required. Structured reflection respects visibility. An inference/expansion
cycle needs an explicit phase boundary or annotation, not guessed types.
Exact helper and macro spellings remain implementation work.

Controlled build-input reads (provisionally `build.input_file`) record
content dependencies. Build options and target metadata are fixed inputs;
environment values must be explicit inputs. Network access and external
commands belong to build steps producing fixed artifact inputs. Users need
not duplicate discovered file dependencies in a manual manifest. Ordinary pure
compile-time execution cannot hide arbitrary I/O.

Effects describe actual resources, keys or ranges, and access modes. Data and
control dependencies also constrain order. Overlapping conflicting accesses
retain source order; unknown overlap is not independence, even through aliases.

A function body expresses data, control, and effect dependencies, not just an
ordered list of commands. Dependency-ready execution is immediate by default;
this meaning does not require a runtime graph. Lazy, unordered, or parallel
transformations require validated equivalence evidence, including error
behavior, termination behavior, early exits, and cleanup.

## Memory, resources, and async

Automatic GC may move objects; source cannot observe addresses or movement.
Relocation preserves logical equality, contracts, and concept behavior.
OOM is an unrecoverable process-level fault. GC provides no finalizers or weak
references. A future FFI uses copying or an explicit pin boundary.
Loom introduces no Rust-style ownership, borrow, lifetime, or `Pin` syntax.

`scoped` and `defer` register cleanup in their containing lexical block,
including an `if` or `else` block. Its actions run once, in LIFO order when that
block exits, including error exits and task cancellation. `MustScope` resources
cannot be discarded. A non-Unit temporary must be used or explicitly discarded;
ordinary discardable values remain discardable. Live Task obligations likewise
cannot be discarded. External resources do not rely on GC cleanup.

Stackless coroutines are lowered into state machines by Loom's own MIR.
Suspension uses postfix `.await`, including chaining with result propagation.
`Task` supports structured async work. Tuple joins preserve heterogeneous
results; List joins support dynamically sized homogeneous task sets. Join
policies belong to `std`, over narrow scheduler primitives. A Task is not a
durable workflow, deployment record, or persistent execution receipt.

## Library boundary and acceptance stories

The runtime supplies GC, scheduling, and narrow platform/storage primitives.
Public library policy and algorithms are Loom source. JSON is a library data
format, not a compiler/runtime special case. Reachability and specialization
must remove unused library code and data.

Acceptance must cover complete stories, not isolated syntax demonstrations:

- An application uses directory packages, private helpers, colocated tests,
  constrained inputs, resources, async I/O, and real check/build/test/run.
- A shared fixed-shape view survives source resizing, while invalidating
  element aliases are rejected for a content-constrained view.
- Sorting contracts prove only their declared normal-return properties;
  unresolved proofs block builds without inventing termination guarantees.
- A source compiler uses generic algorithms, known functions and closures,
  typed macros, and tracked build inputs without compiler-specific shortcuts.
- Published compiler data remains valid across passes and suspension; mutable
  drafts cannot silently invalidate another pass's constrained shared data.

The [roadmap](../../ROADMAP.md) describes staged delivery and bootstrap work.
Stages establish executable evidence without lowering the final requirements.
[Change and deployment](change-and-deployment.md) covers semantic changes and
world-state operations; those concerns must not expand ordinary Task semantics.
