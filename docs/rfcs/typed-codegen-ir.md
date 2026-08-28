# Typed code generation IR

Status: **Accepted; implementation in progress**

## Decision

Loom will place a target-aware, typed SSA representation between checked MIR
and native target emitters. This representation is Loom Codegen IR (LCIR).
Checked MIR remains the portable semantic boundary shared with the interpreter;
LCIR owns callable instances, physical value representations, explicit control
flow, and the information needed for mechanical target emission.

LCIR is compiler-private. Its Rust API, textual dump, physical ABI, and object
symbols may change with the compiler build. It is not a source IR, a stable
artifact format, an ownership system, or a public FFI ABI.

The direct foundation and its first production route are described in the
current [Code generation IR internals](../internals/codegen-ir.md). Ordinary
native build, run, and test preparation now selects complete supported
primitive, direct literal/concat/get Text, structural-tuple, closed-record, and
compile-time-established refined artifacts, plus bounded concrete direct
generic instances over those representations and eligible concrete
closed-enum artifacts including managed Text payloads, lexical cleanup, and
the supported source-contract subset, plus checked stackless coroutines with
typed Task handles, `Task.sleep`, and nonempty static forms of the standard
`Task.all`, `Task.any`, `Task.settled`, and `Task.race` APIs into typed LCIR.
Coroutine coverage includes exact terminal-outcome capture, static lexical
cleanup across suspension and cancellation, plus synchronous functional inout
calls from coroutine bodies. Reachable unsupported features fall back atomically.
The broader representation migration and legacy deletion gates in this record
are not complete.

## Motivation

The production LLVM backend still lowers artifacts outside current direct LCIR
coverage through a universal value implementation and several closed-world
native specializations. Managed values other than direct Text values and
Text-bearing products/sums,
unsupported or recursive enums,
runtime-checked constraints and dynamic concepts,
cleanup shapes outside the direct lexical slice, async shapes outside the
checked coroutine slice, and private-list paths still repeat representation, proof,
call-compatibility, and runtime-requirement decisions inside the legacy target
emitter. Some legacy functions may acquire universal, checked-native, and
assumption-specialized bodies.

That structure makes a correct fast path depend on exact MIR shapes and makes
each additional type or operation multiply the number of lowering choices.
LLVM should receive already selected types, control flow, and proofs instead of
being asked to recover them from a tagged value representation.

## Accepted pipeline

The native pipeline will become:

```text
checked MIR
  -> command roots and closed-world reachability
  -> callable-instance plan
  -> target-aware representation plan
  -> LCIR construction
  -> independent LCIR validation
  -> checked LCIR
  -> target emitter
  -> target verification and optimization
  -> relocatable object
```

Each stage has one responsibility:

| Stage | Responsibility |
| --- | --- |
| checked MIR | Valid source-level types and executable semantics. |
| instance plan | The complete callable graph required by the selected roots. |
| representation plan | One physical representation for every live value and callable boundary. |
| LCIR construction | Explicit typed SSA instructions, blocks, parameters, edges, and origins. |
| LCIR validation | Structural, type, dominance, effect, control-flow, and representation invariants. |
| target emitter | Mechanical translation of checked LCIR to target IR. |

Target emission must not resolve names, reinterpret contracts, infer a physical
representation from an expression shape, or create new callable instances.

The implemented instance planner closes direct calls from the selected
exported run or test roots. Identity contains the source function, exact type
arguments, complete static witness arguments, and the source-contract boundary
role. Ordinary synchronous calls target an `AssumedBody`; an exported
synchronous root with preconditions receives a same-signature `CheckedRoot`
wrapper. An async instance checks its own preconditions in state zero. The planner deduplicates identical
instances across roots, permits exact regular recursion, and rejects
nonregular recursion or finite planning-budget exhaustion before LCIR
construction. Proof-only witness identity remains compile-time data; it does
not force a runtime witness parameter. The implemented concrete static slice
infers a selected conformance head from checked dispatch type, appends exact
method type/proof arguments, normalizes associated projections, and closes the
witness method as an ordinary direct edge. Dynamic calls remain a later
whole-artifact slice.

For a statically established source predicate, fresh checked MIR is also the
process-local proof boundary. The public raw LCIR builder cannot mint
proof-bearing instructions, and LCIR validation checks their exact typed
construction shape; it does not claim to reconstruct and re-prove a predicate
that LCIR does not encode. Serialization replaces `Proven` with `Recheck`, and
decoding normalizes a forged `Proven` spelling the same way. Supported
nongeneric `Recheck` executes the predicate or invariant in typed LCIR before
publishing a nominal value and raises the canonical
`ArtifactProofRejected` runtime fault when replay fails. Generic or otherwise
unsupported replay selects atomic legacy fallback. A checked-MIR wrapper is
neither a portable proof certificate nor publisher authentication.

## Representation policy

Statically known values use direct representations by default. The implemented
vocabulary is:

- `Uninhabited` as control-flow vocabulary for `Never`, never as an SSA value;
- `Zst` for `Unit`;
- `Scalar(I1)` for `Bool`;
- `Scalar(I64)` for `Int`;
- `Scalar(F64)` for `Float`;
- one opaque `ImmortalText` pointer for a 64-bit closed artifact in which every
  text value is transitively produced by a compiler-emitted process-lifetime
  literal;
- one opaque `ManagedPointer` for every Text in an artifact where concat or a
  Text-bearing product is reachable; literals remain static objects and concat
  results are typed moving-GC leaves in the same direct pointer ABI;
- `Product(element value types...)` for a structural tuple whose transitive
  elements are direct values;
- `Product(field value types...)` for a closed, invariant-free record whose
  transitive fields are direct values;
- a protected `Product(field value types...)` for a closed record whose
  invariant was proved statically;
- a distinct semantic value type sharing its established base representation
  for a monomorphic refined type;
- `Sum(variants...)` for a concrete closed enum whose substituted payloads are
  direct values. One variant is tagless, an all-empty multi-variant sum is tag
  only, and every other sum has a minimal tag plus an exact aligned carrier.
- `TaskHandle` for one stable scheduler-owned `Task[T]` pointer in the checked
  coroutine slice. It is not a moving-GC reference or a universal value.

Products and sums are immutable register aggregates. Tuples and records may
contain one another and managed Text leaves when the resulting by-value graph is
acyclic. `ManagedPointer` is the artifact-wide Text provenance mode; products
and closed sums containing such leaves remain unboxed exact aggregates.
Transparent/refined carriers remain pointer-free in this slice. Each
representation plan has an explicit canonical
registration key for semantic-type lookup; value-representation alternatives
are not required to be globally unique by semantic type. General managed,
dynamic-witness, erased, and additional coroutine representations are added
only with complete lowering and validation rules. A generic or dynamic operation
elsewhere in an artifact does not make an unrelated direct value carry a
universal tag.

The direct text slice supports allocation-free length, containment, and
content equality or inequality; equality is never pointer equality. It also
supports concat and Unicode-scalar selection through specialized typed helpers.
Any concat, selection, or Text-bearing product/sum selects `ManagedPointer` for
every Text in the complete artifact; concat and selection add `MAY_COLLECT`.
Exact backwards SSA liveness expands a live aggregate to deterministic guarded
leaf cells and a deduplicated bitmap state for every collecting site. Values
are live after the call, so its not-yet-defined result is excluded; explicit
edge arguments map only to live explicit destination parameters. Empty plans
emit no frame. Other dynamic producers and Text inside transparent/refined
carriers remain whole-artifact fallback. Concrete closed managed Lists are
direct repeated allocations. Literal planning is bounded to
1 MiB of UTF-8 for one literal and 16 MiB across one LCIR artifact.

Concrete structural equality is generated from this same representation plan.
Products compare exact fields, transparent values compare their declared base,
and sums dispatch both tags before observing a matching active payload. Lists
compare length and then canonical `Option[T]` reads in a nonallocating proved
loop. Ordinary expressions and contracts share the lowering. Recursive nominal
equality that re-enters through a List remains atomic fallback until comparison
instances can be recursive without cloning an unbounded CFG.

Transparent representation reuse is not an arbitrary layout cast. The plan
records the exact base type, `RefineProven` requires that base and a distinct
transparent result, and `Unrefine` returns only that base. Likewise,
`InvariantRecordProven` is the only product constructor for a protected record;
ordinary construction and insertion cannot forge its invariant. Unknown
constraint proofs remain normal language `Result` construction and currently
select whole-artifact fallback.

Every representation change is explicit in LCIR. Genuine erased boundaries,
including `dyn C`, use purpose-specific representations. They do not preserve a
universal value envelope for ordinary typed code.

## Whole-artifact migration

Migration selection is atomic for one complete reachable artifact:

1. Compute roots, reachability, target layout, instances, representations, and
   support without mutating an LLVM module.
2. If every reachable item is supported, construct and validate the complete
   LCIR artifact, then emit only the LCIR route.
3. If valid checked MIR contains a reachable feature outside current LCIR
   coverage, emit the complete artifact through the legacy route.
4. Never connect an LCIR function to a legacy function through a universal
   wrapper.

An unsupported unreachable function does not affect route selection. For a
test artifact, the union reachable from every selected test is one artifact;
one unsupported reachable test dependency selects legacy emission for all of
it.

The implementation must distinguish unsupported coverage from defects:

- `Complete` means every reachable callable, witness edge, builtin, root
  harness, representation, and operation is supported.
- `Unsupported` means valid input lies outside migration coverage and permits
  whole-artifact fallback.
- Missing checked-MIR references, inconsistent plans, invalid generated LCIR,
  target-emitter failures, verifier failures, and object-write failures are
  compiler defects and must not fall back.

Route selection and every input that can change it are part of native-object
identity. The backend build fingerprint includes the LCIR implementation.

## Edge-result control flow

A fallible or suspending operation is a terminator, not an ordinary instruction
that returns a value beside an ignorable status. Its source result, or its
ordered child results for a multi-child suspension, exists only on the normal or
resume edge. Ordered functional inout writebacks follow a fallible result on the
normal edge and are also injected on the fault edge. Forwarded edge arguments
are modeled separately from implicit results.

That rule also applies when the caller is a coroutine. A synchronous callee's
normal or fault writebacks replace the corresponding values in the current
coroutine SSA environment. On a fault edge, writeback precedes the active
static cleanup suffix, so cleanup observes the callee's final receiver and
argument values. This does not give the coroutine itself an inout signature or
writeback-bearing Task result.

The scalar fault slice adds forms equivalent to:

```text
checked_int operation, operands
    normal target(result; forwarded...)
    fault target(forwarded...)

invoke callee(arguments...)
    normal target(result, writebacks...; forwarded...)
    unwind target(writebacks...; forwarded...)

assert condition, contract metadata
    success target(forwarded...)
    fault target(forwarded...)

resource_close kind, resource
    normal target(Unit, resource_writeback; forwarded...)
    fault target(resource_writeback; forwarded...)

task.create coroutine(arguments...) -> Task[T]

task.join_all(tasks...) -> Task[(T0, ..., Tn)]

await_tasks all state, (task0, ..., taskN)
    normal target(result0, ..., resultN; exact_live_values...)
    fault target(exact_live_values...)
    cancel target(exact_live_values...)

fault runtime code | contract metadata

resume_fault
```

Checked addition, subtraction, and multiplication map to overflow-aware target
operations. Integer division and remainder first branch on division by zero and
the signed `MIN / -1` case, then execute a proved operation. The two source
faults remain distinguishable.

Contract metadata is authoritative checked LCIR data, not a source lookup. It
contains one canonical assertion/precondition/postcondition/invariant kind,
optional bounded user code, the canonical message, and concrete contract and
blame sources. A synchronous precondition carries its materialized call site;
an async precondition names the creation-site span carried in its coroutine
frame. Every other kind must blame its own contract/assertion span. Independent validation rejects
forged combinations and applies a 4 KiB UTF-8 limit to each text field before
dumping or LLVM global/detail encoding. A synchronous closed-world call
evaluates all arguments, checks `requires` with the call expression as blame,
and enters the assumed body. An async call constructs its child first; the
coroutine checks `requires` in state zero and records failure on that child.
Its compiler-private constructor carries the creation span, while an async root
receives its declaration span from the harness. No source callable accepts a
caller-span argument. Task creation does not inherit the child's fault effect,
and `requires` alone does not make an assumed synchronous body fallible.

An inherent receiver invariant executes at assumed-body entry. Entry values
for `old(self)` and `old(arguments)` remain typed SSA values. On every normal
tail or explicit return, LCIR expands the active lexical cleanup suffix before
checking the current receiver invariant and postconditions. Contract
expressions lower directly from constants, values, bindings, product fields,
unary and checked numeric operations, short-circuit Boolean control flow,
`is_finite`, and bounded exhaustive match DAGs. Arithmetic faults retain their
ordinary `RuntimeFault`; only a false predicate raises its exact contract
metadata.

`defer` and `scoped` are expanded by the MIR-to-LCIR lowerer, not represented by
a runtime cleanup stack. Registration happens at statement reachability, and a
scoped initializer must complete before its disposer becomes active. Each
lexical block expands its suffix newest-first on normal completion, return, and
fault. A first fault remains primary; faults raised by cleanup are suppressed
while every older cleanup still runs. Static-concept disposal uses a closed
typed call with receiver writeback. File and Socket use the typed
`resource_close` edge form. Each `AwaitTasks` terminator has explicit normal,
fault, and cancellation targets that carry one identical exact live row; only
normal receives leading child results. The lowerer expands the active suffix
statically on the other two edges. Child fault activates source fault and ends
in `ResumeFault`. Cancellation preserves inactive source fault and ends in the
coroutine-only `TaskCancelled` terminal. Validation rejects a cleanup call graph
that invents a suspension effect or calls a suspending exact callee, and
cancellation cleanup cannot create, aggregate, or await Tasks, including
through an executor-dependent callee. Active source-fault cleanup cannot await
again. If cancellation cleanup faults, the runtime keeps the established
cancellation primary and continues older cleanup. No runtime cleanup stack or
new runtime ABI is involved.

Validation prevents a checked result from being used on its fault edge and
prevents an unwind edge from returning normally. The target emitter does not
recognize an adjacent instruction-and-branch pattern to recover this meaning.
Function return, fault, and resume-fault exits independently carry the current
values of their declared inout parameters. The same edge-result rule will
apply to suspension and resume data.

## Direct LLVM ABI

The first LLVM emitter uses one compiler-private ABI family for every LCIR
source function:

| LCIR representation | LLVM representation |
| --- | --- |
| `Zst` | empty struct |
| `Scalar(I1)` | `i1` |
| `Scalar(I64)` | `i64` |
| `Scalar(F64)` | `double` |
| `ImmortalText` or `ManagedPointer` Text | opaque pointer |
| `TaskHandle` | opaque scheduler-owned pointer |
| `Product(fields...)` | literal LLVM struct of the recursively mapped fields |
| tagless `Sum` | its sole variant payload struct |
| tag-only `Sum` | its checked minimal integer tag |
| tagged `Sum` | `{ tag, exact target-aligned carrier }` |

An infallible function without inout parameters returns its typed result
directly. With ordered writebacks `W...`, it returns `{ T, W... }`. A function
with the `MAY_FAULT` effect returns `{ i32 status, T, W... }` and receives one
trailing opaque runtime-context pointer. Status zero is success; status one is
a source runtime fault. A normal return supplies the result and current
writebacks. A fault origin reports the fault once and returns status one, a
zero source result, and the current writebacks; an unwind continuation
propagates the failure without reporting it again.

Function effects are an exact compiler-private capability set rather than a
single fallibility flag. `MAY_FAULT` remains independent; checked scalar faults
do not require an active Loom runtime. `MAY_COLLECT` implies `NEEDS_RUNTIME`,
and `MAY_SUSPEND` implies `NEEDS_EXECUTOR`, which implies `NEEDS_RUNTIME`.
Lowering and independent validation separately compute the least transitive
closure over direct and invoke edges. A synchronous caller inherits the effect
of a precondition it evaluates. An async precondition belongs to the child
coroutine's state-zero path, so `TaskCreate` does not inherit child effects.
`TextConcat` and
`TextGet` are collecting opcodes. `TaskCreate` and `TaskJoinAll` require an
executor; neither operation itself suspends. `AwaitTasks` contributes both
`MAY_FAULT` and `MAY_SUSPEND` and accepts one or more ordered children. The
explicit fallible `TaskSleep` terminator requires `MAY_FAULT` and
`NEEDS_EXECUTOR`, but does not itself add `MAY_SUSPEND` or `MAY_COLLECT`.

All functions are declared before bodies are emitted, so direct and mutually
recursive calls use the same typed ABI. Entry block parameters map to function
parameters; non-entry block parameters map to phi nodes. The run or test
harness calls the typed root directly. Product construction and functional
field replacement use `insertvalue`, while projection uses `extractvalue`.
Sum construction and exhaustive switching preserve ordered variants and move
typed payloads through block parameters. `Result[Unit, E]` test roots carry an
explicit success/failure variant plan; the harness never guesses from a source
name or an implicit tag convention.
Tagged carrier conversion is pure SSA bit packing and unpacking at LLVM
target-data offsets on the supported little-endian targets. It does not use a
stack reinterpretation buffer or `memcpy`, including when a live sum crosses a
loop phi. Match plans use IEEE ordered equality for float constants, share one
typed capture block per selected source arm, and are rejected before LCIR
allocation if their bounded pattern, decision, value, work, or CFG-block
budgets are exceeded.
The harness creates no execution runtime for a pure direct root. It may still
declare the output-only `loom_runtime_stdout_write_v1(data, length)` boundary;
that symbol does not add a runtime context, collection capability, or executor
requirement to the source graph. The harness constructs each complete UTF-8
line with a literal LF and supplies its exact byte length, excluding the LLVM
global's trailing NUL. The runtime performs no NUL scan, delimiter insertion,
or C text-mode translation. Output failure may leave a prefix, so generated
code never retries it: an otherwise successful `Unit` or passed-test path
becomes nonzero, while an already failing path retains its nonzero status. A
synchronous faulting or collecting root creates a runtime but no executor. An
async root creates one executor, constructs the root Task, runs it to a terminal
state, takes its exact typed result, and destroys the executor. When the async
root has preconditions, construction supplies its declaration span as
state-zero blame. Both raw object-emission routes independently validate the
complete executable root as `() -> Unit`; the legacy arbitrary-value root
printer is not part of this boundary.

Each admitted async instance has a checked `CoroutinePlan` with an exact output
type, an optional carried creation span for state-zero preconditions, and dense
resume states. Every state records an `AwaitMode`, the exact
output type of every child in task order, and the live value types forwarded to
its continuation. The complete child-output row is independent of the normal
value arity: `all` injects every output, homogeneous `any` injects one
successful output, `settled` injects every terminal child handle, and
homogeneous `race` injects the terminal winner handle.
LLVM target data shapes a frame containing state, parameters, optional caller
span coordinates, one ordered child/live row per suspension, and result. An immutable typed-task descriptor
publishes exact managed-leaf byte offsets and per-state bitmaps. The callback is
used for both the source coroutine's descriptor resume and cancel entries. It
checks the Task cancel request before state dispatch: ordinary state zero enters
the LCIR entry and checks any preconditions before the body, later ordinary
states use the structured join-step ABI, and
cancellation enters the corresponding state-specific cancel edge. Normal join
completion takes the mode-derived typed values into the leading continuation
parameters; child fault and cancellation forward only the identical exact live
row. Completion is published through typed-task v1. `TaskSleep` accepts
normalized `Int` milliseconds only inside that checked coroutine boundary,
returns a first-class typed `Task[Unit]` on its normal edge, and preserves
canonical negative-duration or overflow faults on its fault edge. A source
`Duration` is normalized through product extraction before this terminator.

A coroutine declaration has no functional inout parameters or writeback
result. An explicit mutable coroutine parameter remains unsupported. The body
may nevertheless invoke synchronous functional inout callees: normal and fault
writebacks update its current frame-local environment, with fault writeback
installed before coroutine cleanup. A dynamic View parameter is copied by value
into the Task frame, and mutable dispatch changes only that copy rather than
aliasing the value that created the Task.

When closed-world analysis proves exactly one closed nongeneric witness for a
dynamic concept, planning recursively replaces that View with the witness's
concrete physical type. This admits the value in coroutine parameters, results,
and nested product, sum, and suspension-frame shapes. A finite multi-witness
dynamic View or an open dynamic View has no coroutine-frame representation in
this slice.

An immediately awaited fixed tuple or fixed-argument `Task.all` evaluates its
children left to right and lowers directly to one multi-child `AwaitTasks`, with
no intermediate composite. Its continuation constructs the exact heterogeneous
tuple from the ordered implicit results. A first-class stored fixed
`Task.all` lowers to `TaskJoinAll`, which creates an exact
`Task[(T0, ..., Tn)]`; a one-child join retains its canonical one-field tuple.
LLVM generates one target-laid-out composite frame, completed-result root map,
immutable descriptor, and callback per distinct static result shape. The
callback uses the existing structured `all` join-step protocol, takes exact
child results in order, and publishes the tuple without a universal envelope.

A nonempty, immediately awaited, fixed-arity `Task.any` also lowers directly to
one `AwaitTasks` when every child has the same exact output type. The runtime
keeps the winner's original input ordinal, drains all children, then finalizes
and retires losers exactly once when generated code consumes the valid join
completion. LLVM selects the winner from the original static child fields in the
coroutine frame and performs one exact typed take. A completed loser's disposal
fault reaches the await fault edge before static coroutine cleanup. If no child
succeeds, generated code records canonical `TaskAnyFailed` at the await origin.
Cancellation uses the checked cancel edge instead and does not synthesize that
fault.

Immediately awaited fixed `Task.settled` and `Task.race` calls use the same
private `AwaitTasks` boundary. Their normal edges inject terminal affine child
handles, which explicit collecting `TaskOutcomeTake` instructions consume into
canonical `TaskOutcome[T]` sums. `settled` preserves every child in source
order. `race` retains the first terminal winner and shares generalized loser
finalization with `any`. A sole nonempty List literal is flattened to the same
static child row without constructing the input List; `all` and `settled`
construct their List result after resume. These are compiler specializations of
standard-library APIs, not additional language syntax.

Version 0.3 still selects these specializations by recognizing the qualified
source names. The target pipeline will first perform ordinary standard-library
resolution, then select specialization through stable intrinsic identity on the
resolved declaration. Its minimum private substrate provides typed join/select
readiness, exact result or outcome extraction, and structured
cancellation-and-drain. Public policy names do not become LCIR source operators.

The remaining fallback boundary includes explicit mutable coroutine
parameters, finite-catalog or open dynamic-concept frame values, raw readiness,
empty/stored/computed/runtime-sized Task List joins, and first-class
`Task.any`, `Task.settled`, or `Task.race` results. Concrete closed `List[T]`
and compiler-private `TextMap[V]` values are canonical one-pointer frame
carriers in parameters, results, nested products, and suspension-live rows.
Static joins are admitted only when a nonempty fixed argument row or sole List
literal is consumed immediately by `.await`; `any` and `race` require one exact
output type.

Managed Text concat calls
`loom_runtime_text_concat_typed_v1(left, right, output)`. The helper must stage
and validate both full UTF-8 inputs before any allocation can collect, then
allocate a 32-byte, 8-aligned, pointer-free typed prefix with trailing bytes,
initialize it without a safepoint, and publish the result last. OOM is an
uncatchable process-level fault; every other nonzero status fails closed. LLVM
publishes exact typed-root states before concat and transitively collecting
calls. A direct Text value has one stable cell; a live unboxed product has one
cell per deterministic Text-leaf projection. A closed sum catalogs candidate
cells for every variant, conjoins nested tag predicates, publishes only the
active variant, and clears all inactive candidates. Definitions and phis
publish the leaves, aggregate uses are reconstructed progressively from
post-safepoint reloads, and every nonempty frame is popped on all source exits.
Root-map ABI-limit excess is `ProgramTooLarge`, not fallback. No universal root
chain or executor participates in this path. Product and sum leaf rooting reuse
typed-shadow-stack v1; sum support itself does not advance runtime component 13
or `runtime-v7`.

Typed File and Socket cleanup calls
`loom_runtime_resource_close_typed_v1(runtime, kind, handle_cell)` directly.
It adds `typed-resource-v1` and advances the runtime ABI component to 12 with
`runtime-v6`. The handle cell is a fixed LLVM entry allocation, not a runtime
cleanup node, and the helper neither constructs a universal value nor enters
the executor. Static-concept and deferred cleanup require no new runtime ABI.

The runtime subsequently adds a bounded repeated-element descriptor. This
additive `typed-repeated-v1` symbol advances the
native component to 13 with `runtime-v7` and the collector identity to
`gc-v9`. Monomorphized List lowering consumes it directly while fixed typed
objects retain the existing v1 allocator wire; no later List-specific runtime
ABI change is needed.

A following additive Text helper stages one selected scalar before allocating
its direct result. `loom_runtime_text_get_typed_v1` advances Text identity to
`text-v3` and native component 14 (`runtime-v8`) while retaining `gc-v9`.
Managed `Option[Text]` lowering consumes its found/missing status without a
universal envelope. Missing selection constructs a zero carrier and performs
no allocation; found selection uses the staged direct Text pointer, and an
invalid status traps as an ABI defect.

Typed Float formatting adds
`loom_runtime_format_float_typed_v1(value, out_cell)`. The helper publishes a
canonical direct Text pointer only after initialization and uses one closed
status domain. It advances native component 15 with `format-float-v1` and
`runtime-v9` while retaining `text-v3`, `gc-v9`, and both typed allocation
wires.

Typed sleep adds the narrow
`loom_typed_timer_task_create_v1(executor, deadline_ns)` factory. The compiler
checks millisecond-to-nanosecond multiplication and monotonic deadline addition
before calling it. The runtime creates a zero-root typed `Task[Unit]`, registers
the existing one-shot timer `WaitSource`, publishes Unit after readiness, and
removes the registration on cancellation. This advances native component 16
with `typed-timer-v1` and `runtime-v10`; typed-task v1 and wait v1 remain
unchanged.

Stored fixed `Task.all` adds
`loom_typed_task_publish_adopting_v1(executor, composite, children, count)`.
Generated code first initializes an unpublished exact composite and supplies a
temporary ordered child-pointer array. The runtime validates every ownership
edge and completes all fallible reservations before one allocation-free commit
transfers the children from the active parent, publishes the composite, and
queues it. Failure leaves ownership and queue topology unchanged; generated
code aborts the unpublished composite and fails closed. This advances native
component 17 with `typed-task-adopt-v1` and `runtime-v11`, while
`typed-task-v1`, `typed-timer-v1`, `wait-v1`, and `gc-v9` remain unchanged.

Static heterogeneous joins first advanced the canonical dump to `lcir 26`, the
artifact identity to schema 27, the LCIR native-object domain to
`loom-lcir-native-object-v23`, and the CLI object-cache domain to
`loom-llvm-object-cache-v28`. Explicit async cleanup and cancellation exits then
advance those compiler-private boundaries to `lcir 27`, schema 28,
`loom-lcir-native-object-v24`, and `loom-llvm-object-cache-v29`. That slice
leaves the runtime at native component 17 with `runtime-v11`.

Direct fixed `Task.any` advances the canonical dump to `lcir 28`, the artifact
identity to schema 29, the LCIR native-object domain to
`loom-lcir-native-object-v25`, and the CLI object-cache domain to
`loom-llvm-object-cache-v30`. It adds no symbol and does not revise the typed-task
or coroutine wire, but consuming `loom_task_join_step` now finalizes typed losers
before exposing the step. The exact native runtime identity therefore advances
to component 18 with `typed-task-any-finalize-v1` and `runtime-v12`.

Static `settled`/`race`, literal-List specialization, and explicit outcome
transfer advance the canonical dump to `lcir 29`, artifact identity to schema
30, LCIR native-object domain to `loom-lcir-native-object-v26`, and CLI
object-cache domain to `loom-llvm-object-cache-v31`. The additive
`loom_typed_task_take_outcome_v1` boundary and generalized `any`/`race` winner
finalization advance the exact native runtime identity to component 19 with
`typed-task-winner-finalize-v1`, `typed-task-outcome-v1`, and `runtime-v13`.
Typed-task v1, coroutine v2, wait v1, and GC v9 remain unchanged.

Async state-zero preconditions and coroutine-carried creation-site blame advance
the canonical dump to `lcir 30`, artifact identity to schema 31, LCIR
native-object domain to `loom-lcir-native-object-v27`, and CLI object-cache
domain to `loom-llvm-object-cache-v32`. The checked plan and contract metadata
encode the optional caller span; the root harness supplies the declaration span
when no creating call exists. Existing fault-context entry points and the JSON
wire schema are reused, so native runtime component 19 and `runtime-v13` do not
change.

Exact native harness stdout subsequently adds
`loom_runtime_stdout_write_v1(data, length)` and advances the native runtime
identity to component 20 with `stdout-v1` and `runtime-v14`. This is an
output-only symbol, not an execution-runtime requirement. LCIR serialization
and artifact schema do not change because harness output is not part of LCIR.
Native-object and object-cache domains do not advance because the exact runtime
identity already participates in object fingerprints and runtime-bundle
validation.

Calls to the C process entry, libc, and versioned Loom runtime functions are
explicit external boundaries. They do not permit two source-function ABIs in
one artifact.

## Target-emitter rules

The emitter consumes only checked LCIR and validated roots. It does not receive
MIR, universal value slots, argument-node lists, native-range plans, or legacy
runtime-requirement graphs.

It creates all functions and blocks first, translates values in SSA, records
edge inputs, and completes phi nodes after terminators are emitted. If two
edges from one source block target the same destination, normalization or an
edge block gives each phi input a unique LLVM predecessor. Ordinary direct
function bodies do not allocate universal stack slots or private record
storage.

Proved integer add, subtract, and multiply may use LLVM's `nsw` operations.
The first such primitive is an integer successor carrying the exact comparison
fact `value < upper_bound`. Validation requires that comparison's unique true
edge to dominate the successor. Because every signed `Int` upper bound is at
most `MAX`, the fact implies `value <= MAX - 1`; `value + 1` is therefore
representable. A false edge, bypass edge, or control-flow join loses the fact.
This is a general LCIR theorem, not recognition of a source range or Fibonacci
shape. Checked operations use signed overflow intrinsics. Float operations do
not use fast-math flags. Branch likelihood is explicit LCIR metadata or part of
a specific checked/invoke terminator; the emitter does not infer it from a body
shape.

Target-machine creation, module triple and data layout, debug-source mapping,
LLVM verification, optimization, object writing, runtime-bundle linking, and
host linking remain shared infrastructure.

## Migration and deletion gates

Legacy implementation is removed by demonstrated semantic coverage:

| Legacy area | Gate before deletion |
| --- | --- |
| Scalar native wrappers and universal scalar locals | LCIR covers reachable scalar signatures, locals, CFG, direct and fallible calls, checked faults, scalar contracts and cleanup exits, and run/test harnesses with zero eligible fallback. |
| Assumed integer bodies | General LCIR proofs preserve checked behavior inside and outside proved domains, recursive benchmarks remain competitive, and no emitted body depends on one recursion pattern. |
| Duplicated universal/native/assumed requirement scans | Runtime, collection, executor, and fault requirements are derived from checked LCIR and its closed instance graph. |
| Aggregate and private-storage specializations | Direct products cover structural tuple construction and destructuring, tuple/record nesting, closed POD record construction, copy/move, bounded typed nested places, functional mutation, aggregate phis/loops, typed calls, and whole/projected invariant-free record receiver writeback on normal and fault edges. Direct sums cover closed concrete enum construction, ordered exhaustive match decisions, typed payload edges, nested products/sums, and `Result` test outcomes. General managed representation, escape, range, and scalar-replacement planning must still cover GC, cleanup, suspension, and protected or managed projected behavior. |
| Universal function ABI and `ValueSlot` | LCIR covers aggregates, enums, refined values, generics, witnesses, `dyn`, contracts, builtins, moving GC, cleanup, async functions, Tasks, and all maintained native fixtures without fallback. |

New exact-shape native specializations are not accepted migration work. Range,
escape, liveness, devirtualization, and scalar-replacement facts belong in
planning or validated LCIR transforms.

## Acceptance criteria

Every LCIR slice requires:

- builder and independent validator tests, including malformed programs;
- stable dumps for the same checked program and insertion order;
- source-to-MIR-to-LCIR tests for every newly supported operation;
- interpreter/native differential tests for results and faults;
- LLVM structure tests for direct signatures, phi nodes, calls, checked
  operations, and fault propagation;
- negative IR assertions that a complete direct artifact has no universal
  value, linked argument nodes, legacy tag operations, or universal root
  wrapper; synchronous slices also require no executor, while async slices
  require only the checked typed-task/executor surface;
- reachable-unsupported and unreachable-unsupported route tests;
- tests proving that lowering, validation, and target defects do not fall back;
- development and release verification on every supported native CI host;
- object-identity tests for LCIR format, planning, target-layout, root,
  reachability, or route-selection changes;
- controlled direct-value benchmarks against the legacy compiler and the maintained
  Go, Rust, C, and C++ cases.

The migration is complete only after the legacy universal source-function ABI
and its exact-shape specializations are deleted. LLVM optimization output alone
is not evidence that typed lowering is complete.

## Non-goals

This record does not add source syntax, ownership or borrowing, AST editing,
live programming, AOP, runtime conformance search, operator dispatch, a stable
native library ABI, or a public serialization format for LCIR.
