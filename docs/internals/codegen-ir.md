# Code generation IR

`loom-codegen-ir` owns two code-generation boundaries. Its source-graph module
selects checked-MIR function roots and computes the closed-world source graph
used by production native compilation. Separately, its LCIR foundation
provides target-aware scalar, direct Text, closed-product, closed-sum,
transparent nominal, managed List, and compiler-private typed TextMap
representations, whole-artifact
checked-MIR lowering, typed SSA data structures, builders, independent program
and artifact-root validators, and a textual dump for tests and review.

`loom-codegen-llvm` consumes the resulting `CheckedArtifact` directly and emits
its typed functions and run/test harness without the universal value ABI or
an executor. Its production prepared router attempts that whole-artifact
lowering once. `Complete` selects only typed LCIR; only `Unsupported` stores a
source reachability graph and selects the complete legacy emitter. Both routes
have independent object identities. The remaining LCIR coverage and deletion
gates are in the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md).

LCIR is compiler-private and target-specific. It is not a source IR, a public
artifact format, or a stable native ABI.

## Checked-MIR source graph

`SourceRoots` contains MIR `FunctionId` values selected for one command.
`analyze_source_reachability` closes direct calls, constructed witnesses,
dynamic requirement slots, and builtins into a deterministic
`ReachableSourceGraph`. These names deliberately include “source”: lowered
artifact roots use LCIR `InstanceId` values and are a different graph.
Root selection and graph analysis require `loom_mir::CheckedProgram`; this
module has no public raw-MIR compatibility entry point.

The graph records only ordered maps and sets and retains its existing Serde
field order because it participates in native-object fingerprints. Invalid
MIR references discovered while closing caller-supplied roots produce a
structured `GraphError`; the LLVM boundary maps that error into its backend
diagnostic without making source-graph analysis depend on LLVM. References
inside the program have already crossed the independent MIR validator.

## Current direct representation catalog

`TargetLayout` currently records only pointer width. LLVM target data supplies
the ABI layout of direct register products; a future representation with an
explicit byte or address-space layout must add its deciding facts here. The canonical
`RepresentationPlan` contains:

| Loom type | LCIR representation |
| --- | --- |
| `Never` | `Uninhabited` |
| `Unit` | `Zst` |
| `Bool` | `Scalar(I1)` |
| `Int` | `Scalar(I64)` |
| `Float` | `Scalar(F64)` |
| literal-only `Text` on a 64-bit target | `ImmortalText`, one opaque pointer |
| artifact containing `Text.concat` or a Text-bearing product on a 64-bit target | `ManagedPointer`, one opaque pointer for every Text |
| structural tuple | `Product(element value types...)` |
| closed invariant-free record | `Product(field value types...)` |
| closed record with a proven invariant | protected `Product(field value types...)` |
| established monomorphic refined type | its base `ReprId`, with a distinct nominal `ValueTypeId` |
| one-variant closed enum | tagless `Sum(variant payload...)` |
| multi-variant closed enum with no payload fields | minimal integer tag |
| other closed concrete enum | `{ minimal integer tag, exact aligned payload carrier }` |
| concrete closed `List[T]` on a 64-bit target | `ManagedPointer`, one opaque pointer to typed repeated storage |
| concrete closed `TextMap[V]` on a 64-bit target | `ManagedPointer`, one opaque pointer to typed repeated entry storage |

`Uninhabited` is catalog vocabulary only. The validator rejects it in function
signatures and SSA values. Products and sums are immutable register aggregates.
Their fields may be primitive values or other acyclic direct aggregates, so
tuples, records, and closed sums may contain one another. Products and sums may
additionally contain managed Text leaves; transparent/refined carriers remain
pointer-free.
Concrete instantiations of generic enums, including `Result[Unit, E]`, are
eligible after payload substitution. Proven monomorphic refined values and
closed records with statically proven invariants may appear as product fields
or sum payloads. Fully concrete generic records use the same plan.
Runtime-checked constructions, recursive
sums, Task, dynamic witnesses, and uninhabited fields are not selected. A
concrete List or TextMap breaks by-value aggregate recursion and may contain any
registered closed direct scalar, Text, product, sum, List, or TextMap value.
Every TextMap also has managed Text keys. Managed Text is admitted through
product fields, closed sum variants, List elements, and TextMap keys or values,
but not transparent/refined carriers. `InvariantRecordProven` is the only
construction for an invariant product; `RefineProven` and exact `Unrefine`
preserve the physical SSA value while retaining the proof boundary.

`ImmortalText` is deliberately not a general managed representation. Its only
producer is an LCIR `TextLiteral`, which points at an immutable,
compiler-emitted object that lives for the process lifetime. A run or test root
has no parameters, all source functions in a checked artifact have internal
linkage, and the artifact validator requires the exact direct-call closure.
Consequently, a `Text` parameter or block parameter in this slice can only
receive a value derived transitively from a literal in that same closed
artifact; no external or moving pointer can enter the closure. Text may flow
through locals, block parameters, direct calls, returns, and concrete generic
identity functions. It cannot appear in products, sums, or transparent
representations.

If any reachable function uses concat or scalar selection, or places Text in a
tuple/record product or closed sum,
the canonical Text registration instead uses `ManagedPointer` throughout the
artifact. Literals remain immutable process-lifetime objects in that direct
pointer ABI, while concat results are typed moving-GC leaves. The product is
still an unboxed exact SSA aggregate; `ManagedPointer` describes only its Text
leaf provenance. `TextConcat` and successful `TextGet` are infallible
`MAY_COLLECT` operations; exact
live-after SSA liveness and the typed shadow stack let the collector rewrite
direct pointers at its safepoints. Allocation-free `TextLength`, `TextContains`,
and `TextCompare` work in either mode, and equality compares content, never
object addresses. `TextGet` returns the canonical closed `Option[Text]` sum;
negative or out-of-range indices select `None` without allocating, while a
found Unicode scalar is allocated through the typed helper. Other dynamic Text
producers and Text inside transparent/refined carriers still select atomic
whole-artifact fallback.

Text planning is bounded before LCIR allocation or source storage is cloned.
One UTF-8 literal may contain at most 1 MiB, and all literal instructions in
one artifact may contain at most 16 MiB in total. Crossing either bound is
unsupported coverage and selects the complete legacy route. Independent LCIR
validation repeats both limits before LLVM constructs any constant object.

## Structural equality lowering

Equality is expanded from the checked semantic type and its exact `ValueType`
plan; LCIR does not add a universal comparison instruction or a runtime type
switch. `Unit` is always equal. Bool and Int use typed integer comparisons,
Float uses IEEE ordered equality, and Text uses content-based `TextCompare`.
Transparent refined values are unrefined only to their declared base. Products
then compare fields in representation order. Closed sums switch over the left
and right tags before comparing a matching variant's payload, so generated code
never reads inactive carrier bytes. Inequality negates the complete equality
result rather than changing component semantics. The same expansion is used
for ordinary expressions and checked `requires`/`ensures` expressions.

A concrete `List[T]` first compares lengths, then walks equal-length inputs by
an Int index. Each iteration uses existing nonallocating `ListGet` operations
and compares their canonical `Option[T]` results structurally. The loop
backedge uses `IntSuccessorBelow` with the exact `index < length` true-edge
proof, so it adds neither a checked-overflow fault nor a runtime helper. Reads
create no alias-visible mutation and cross no collection safepoint; List value
semantics and typed GC roots are unchanged.

Planning bounds the expanded equality CFG to 4,096 structural units and
registers every implicit `Option[T]` before LCIR construction. Re-entering one
nominal type through a List remains whole-artifact unsupported: inlining that
coinductive semantic equality would make an unbounded CFG. A future reusable
recursive comparison-instance plan can close that case without changing the
source equality rule.

Support classification first builds one concrete aggregate plan, without
allocating LCIR. The plan covers every reachable structural tuple, closed
record, concrete closed enum, and transparent refined chain, orders
registrations after their direct-value dependencies, and rejects mixed
product/sum/transparent by-value cycles. Classification walks each candidate
aggregate graph iteratively. Before substituting or cloning a generic payload,
it walks borrowed declarations and rejects any reachable by-value nominal
cycle by `TypeId`; cached acyclic declarations may then appear repeatedly at
different concrete arguments, such as `Option[Option[Int]]`. Before allocating
the variant table it also reserves `1 + variants + payload occurrences` from
the structural budget. These preflights prevent recursive substitution and
wide tag-only enums from allocating an unbounded intermediate plan. The
preflight and concrete walks both enforce a 256-node type budget, and the
concrete walk also limits nesting depth to 256.
Structural size counts every aggregate occurrence, sum variant, and payload or
product field occurrence. A wide tuple, record, or enum and repeated nested
aggregates therefore consume the same finite budget as a deep chain. Crossing
either limit is stable unsupported
coverage and selects the atomic legacy route; it is not a lowering defect and
cannot consume the compiler's call stack. Independent LCIR validation enforces
the same limits for explicit builder clients.

Projected-place preflight is independently bounded. One place may cross at
most 64 record fields, and the complete artifact may request at most 65,536
units of aggregate extraction/reconstruction work. Reads and moves charge one
unit per field, writes charge the forward extraction plus reverse insertion,
and inout calls reserve reconstruction on both normal and fault edges. An
invalid field path, a path through a protected or managed parent, excessive
depth, or exhausted work budget produces `Unsupported(ProjectedPlace)` during
classification. The whole artifact then selects the legacy route before any
LCIR value or block is allocated.

`ValueType` entries are representation alternatives, not a global uniqueness
claim for a semantic type. A separate canonical registration table selects the
ordinary SSA value representation used by this plan. This permits later plans
to add another representation for the same semantic type without making
semantic type equality an accidental layout key. The plan maintains a
deterministic ordered map for logarithmic canonical lookup; validation rebuilds
that map from the ordered registrations and rejects a duplicate or stale
index. Every alternative for one semantic type must inherit the canonical
construction protection. An invariant product cannot acquire a direct
alternative, and every transparent alternative must retain the canonical
base semantic relation even when a future plan chooses a different physical
representation.

`TargetLayout::new` accepts nonzero, byte-sized pointer widths no greater than
128 bits. Acceptance by this standalone type is not a Loom native-target claim;
the production LLVM backend and runtime retain their own supported-target
boundary.

## Current program model

`ProgramBuilder` declares functions and produces an unchecked `Program`.
`Program::into_checked`, `ProgramBuilder::finish_checked`, and
`check_program` cross the independent validation boundary and return a
`CheckedProgram`.

`ArtifactRootRequest` selects either one run root or an ordered, possibly
empty test-root list. Every test root has an explicit `TestOutcomePlan`: `Unit`
or the success and failure variant indices of `Result[Unit, E]`.
`check_artifact` independently checks branded function identity, existence,
duplicate tests, the zero-parameter root signature and outcome shape, and
exact direct/invoke callable closure. It then returns a `CheckedArtifact`
which owns both the checked program and privately checked roots. The independent
LLVM object API consumes that wrapper without accepting unchecked roots or
falling back to checked MIR.

`artifact_identity` and `write_artifact_identity` expose a deterministic,
compiler-private identity for that complete checked artifact. Schema 8 carries
the `typed-lcir-whole-artifact` route tag, artifact kind, ordered run or test
roots, and the canonical LCIR dump with origins enabled. The payload therefore
includes the target, representation, and instance plans, checked functions and
control flow, operations, and complete function, instruction, and terminator
origins.
The dump uses explicit enum spellings and string escaping rather than Rust
`Debug`. Dense numeric IDs are content, but the process-local generative
`ProgramBrand` is deliberately excluded, so independently built artifacts with
the same deterministic numbering and content have the same identity. The
production LCIR fingerprint streams this identity together with backend,
target-machine, optimization, runtime ABI, and debug-source identities.

The callable-instance plan introduced artifact-identity schema 2 without
changing the emitted machine ABI. Direct products, inout writebacks, and their
operations changed the encoded LCIR meaning and advanced the identity to
schema 3 and the text dump to `lcir 2`. They also changed the emitted machine
ABI, so the independent native-object format advanced to
`loom-lcir-native-object-v2` and the CLI object-cache domain to
`loom-llvm-object-cache-v7`. Explicit function entries and checked types on
every instruction result advance the identity to schema 4 and the text dump to
`lcir 3`. Reusing the instance-key type encoder for every representation and
registration advances the identity to schema 5 and the text dump to `lcir 4`;
new direct tuple entries and future nominal-argument, task, view, and other type
entries cannot collapse to a shared placeholder. Tuple lowering therefore
reuses schema 5 and dump version 4: its complete semantic identity was already
encoded before the representation became selectable. Transparent value
provenance, its explicit proof operations, and explicit test outcome plans
advance the artifact identity to schema 6 and the dump to `lcir 5`;
transparent values and protected invariant records reuse their base/product
ABIs. Closed-sum representation and control-flow semantics then advanced the
identity to schema 7 and the dump to `lcir 6`. Sums added a new physical ABI,
including when transparent or protected products were payloads. At that point,
the LCIR native-object format became `loom-lcir-native-object-v3`, and the CLI
cache domain became `loom-llvm-object-cache-v8`.
Concrete generic-instance closure reuses those versions: the existing instance
plan, canonical dump, and schema-7 identity already encode every exact type and
witness argument, function body, signature, and call edge. The backend build
fingerprint invalidates objects when the planner implementation changes. No
serialized grammar or physical ABI changed, so the text, native-object, and
object-cache domains do not advance again.
Literal-only `ImmortalText`, its operations, and its one-pointer callable ABI
then advance the artifact identity to schema 8, the dump to `lcir 7`, the LCIR
native-object domain to `loom-lcir-native-object-v4`, and the CLI object-cache
domain to `loom-llvm-object-cache-v9`. The emitted constants use the existing
native text layout descriptor and containment-helper symbols. The native
runtime ABI is therefore unchanged.
The explicit transitive effect lattice then advances the artifact identity to
schema 9, the dump to `lcir 8`, the LCIR native-object domain to
`loom-lcir-native-object-v5`, and the CLI object-cache domain to
`loom-llvm-object-cache-v10`. It changes compiler-private planning identity but
introduces no runtime ABI symbol or physical value layout.
Exact typed fault metadata then advances the artifact identity to schema 10,
the dump to `lcir 9`, the LCIR native-object domain to
`loom-lcir-native-object-v6`, and the CLI object-cache domain to
`loom-llvm-object-cache-v11`. The encoding replaces generic assertion and
contract placeholders with canonical fault kind, category, bounded user code,
message, contract span, and concrete blame span. It changes no runtime ABI
symbol or physical value layout.
Concrete static concept dispatch and associated-projection normalization then
advance the artifact identity to schema 11, the dump to `lcir 10`, the LCIR
native-object domain to `loom-lcir-native-object-v7`, and the CLI object-cache
domain to `loom-llvm-object-cache-v12`. Static proof trees remain
compiler-private instance identity and lower to direct calls; the runtime ABI
does not change.
Dynamic `Text.concat` then advances the artifact identity to schema 12, the
dump to `lcir 11`, the LCIR native-object domain to
`loom-lcir-native-object-v8`, and the CLI object-cache domain to
`loom-llvm-object-cache-v13`. An artifact containing concat selects one
`ManagedPointer` representation for every `Text`, including literals. The new
runtime helper and typed shadow-frame calls advance the native runtime ABI
component to 10 and its text/runtime identity components to `text-v2` and
`runtime-v4`; the underlying typed-GC component remains `gc-v8`.
Managed Text leaves in unboxed products then advance the artifact identity to
schema 13, the dump to `lcir 12`, the LCIR native-object domain to
`loom-lcir-native-object-v9`, and the CLI object-cache domain to
`loom-llvm-object-cache-v14`. The existing typed-shadow-stack v1 descriptor,
frame, bitmap, push, and pop wire is sufficient, so native runtime ABI component
11 and its `runtime-v5` identity do not change.
Direct lexical cleanup then advances the artifact identity to schema 14, the
dump to `lcir 13`, the LCIR native-object domain to
`loom-lcir-native-object-v10`, and the CLI object-cache domain to
`loom-llvm-object-cache-v15`. Typed File and Socket disposal adds the
`typed-resource-v1` boundary and advances the native runtime ABI component to
12 with `runtime-v6`; deferred blocks and statically selected concept disposal
need no runtime cleanup representation.
The additive repeated-element allocator then advances only the runtime
boundary: native component 13, `runtime-v7`, `gc-v9`, and
`typed-repeated-v1`. Fixed-offset typed allocations remain `typed-gc-v1`;
monomorphized List lowering consumes the repeated symbol without another
runtime ABI change.
Managed Text leaves in closed unboxed sums then advance the artifact identity
to schema 15, the dump to `lcir 14`, the LCIR native-object domain to
`loom-lcir-native-object-v11`, and the CLI object-cache domain to
`loom-llvm-object-cache-v16`. Candidate root slots are ordered by dense SSA
value and typed product/sum path. Publication conjoins every enclosing tag,
writes null for inactive variants, and reload reconstructs only the active
payload from target-layout byte offsets. This reuses typed-shadow-stack v1 and
does not change native runtime component 13, `runtime-v7`, or `gc-v9`.
Typed scalar selection then adds `loom_runtime_text_get_typed_v1`, advancing
Text to `text-v3` and the native component to 14 with `runtime-v8` while
leaving `gc-v9` unchanged. LCIR consumption advances the artifact identity to
schema 16, the dump to `lcir 15`, the native-object domain to
`loom-lcir-native-object-v12`, and the CLI object-cache domain to
`loom-llvm-object-cache-v17`; the runtime boundary does not change again.
Typed source-contract placement then advances the artifact identity to schema
17, the dump to `lcir 16`, the LCIR native-object domain to
`loom-lcir-native-object-v13`, and the CLI object-cache domain to
`loom-llvm-object-cache-v18`. These domains now encode checked-root versus
assumed-body identity, call-site preconditions, entry and exit invariant
checks, post-cleanup postconditions, and the protected-receiver transient
update form. No runtime symbol, physical value representation, or runtime ABI
component changes.
Monomorphized managed Lists then advance the artifact identity to schema 18,
the dump to `lcir 17`, the native-object domain to
`loom-lcir-native-object-v14`, and the CLI object-cache domain to
`loom-llvm-object-cache-v19`. The existing runtime ABI component 14 and
`typed-repeated-v1` wire are unchanged. Typed nongeneric proof replay then
advances the artifact identity to schema 19, the dump to `lcir 18`, the LCIR
native-object domain to `loom-lcir-native-object-v15`, and the CLI object-cache
domain to `loom-llvm-object-cache-v20`. `Assert` now carries either canonical
contract or runtime fault metadata, allowing `ArtifactProofRejected` to share
the exact typed unwind and lexical-cleanup path. No runtime symbol, physical
value representation, or runtime ABI component changes.
Typed scalar builtins then advance the artifact identity to schema 20, the
dump to `lcir 19`, the LCIR native-object domain to
`loom-lcir-native-object-v16`, and the CLI object-cache domain to
`loom-llvm-object-cache-v21`. `ParseInt` and `ParseFloat` reuse their existing
closed status boundaries. `IsFinite` and `Duration` expand into typed LCIR;
negative Duration construction uses the canonical runtime-fault `Assert`
path. `FormatFloat` adds `loom_runtime_format_float_typed_v1`, advancing the
native runtime component to 15 with `format-float-v1` and `runtime-v9` while
retaining `text-v3`, `gc-v9`, and the existing typed allocation wires.

`lower_typed_artifact` accepts a checked MIR program, a source run/test
request, and a target layout. It first selects the exported run root or ordered
test roots, validates their source reachability, then closes exact concrete
function instances before classifying any of them. Classification covers the
entire instance and representation plan before allocating LCIR. It returns
either one complete independently checked
`CheckedArtifact` or one deterministic `SupportReport` for the whole artifact.
Invalid roots, resource limits, source-graph defects, and invalid generated
LCIR are structured `LoweringError` values and never select fallback.

The current lowering coverage is synchronous scalar, direct `Text`,
structural tuple, closed-record, concrete closed-enum, and established refined
signatures, including bounded direct generic calls whose concrete types use
those representations. Concrete static concept calls use the selected witness
method directly, including conditional proof applications and normalized
associated bindings. Dynamic dispatch remains a whole-artifact fallback. It
covers constants, locals and assignment, tuple construction
and immutable `let` destructuring, blocks and conditionals,
short-circuit Boolean operations, integer ranges, pure scalar operations,
checked integer arithmetic, and direct/readonly-inherent calls including
recursion. Finite checks, integer/float parsing, float formatting, and Duration
construction/extraction also lower directly. Plain record construction,
whole-value copy and move, nested field
read/write, tuple/record nesting, product block parameters, parameters,
returns, and loop-carried products lower directly to SSA. Compile-time-proven
refined construction, exact unrefinement, and compile-time-proven record
invariants are representation-preserving typed operations. Unknown refined
predicates and record invariants remain normal `Result[..., ConstraintError]`
constructions and select whole-artifact fallback. A portable MIR proof replay
(`ConstructionMode::Recheck`) for a nongeneric refined type or invariant
record re-evaluates the embedded predicate in typed LCIR, raises the canonical
`ArtifactProofRejected` runtime fault on rejection, and creates the established
nominal value only in the accepted block. Generic proof replay remains an
explicit `SerializedProofRecheck` fallback. Enum construction uses
`SumConstruct`. Exhaustive matches lower through a bounded decision DAG
which preserves source arm order, evaluates the scrutinee once, compares scalar
subpatterns only where needed, and emits an exhaustive `SumSwitch` with typed
payload edge parameters at each sum decision. Every selected source arm has
one shared LCIR block with typed capture parameters, so multiple DAG paths do
not duplicate its body. A generic body's plan is keyed by its exact concrete
`InstanceKey`, so separate instantiations derive distinct payload and capture
types. Float-pattern equality is IEEE ordered equality:
`+0.0` and `-0.0` select the same constant arm, while a NaN pattern can never
match and is removed from the decision plan. Pattern, decision-node, and
abstract-value budgets are each 512, planning work is limited to 32,768 units,
and the complete match may require at most 1,024 CFG blocks including its join.
All limits are checked before the lowerer allocates any match LCIR; exceeding a
limit selects whole-artifact fallback. A mutable inherent
receiver is a functional inout parameter:
the callee returns its current product on both normal and fault exits. A direct
mutable inherent call may also borrow an invariant-free record at a projected
place when the leaf has the exact receiver type. Its leaf writeback is rebuilt
into the current aggregate root on both exits; unsupported receiver shapes
select atomic fallback.
A dense reverse-call worklist computes the least transitive effect fixed point
in linear time and chooses direct calls versus fallible invokes. The effect
set has independent `MAY_FAULT`, `NEEDS_RUNTIME`, `MAY_COLLECT`,
`NEEDS_EXECUTOR`, and `MAY_SUSPEND` capabilities. Collection implies an active
runtime; suspension implies an executor, which implies an active runtime.
`MAY_FAULT` intentionally implies none of those capabilities because checked
scalar faults use only the local fault context. A caller gains `MAY_FAULT`
when it executes an unknown precondition; the assumed callee body does not gain
that effect merely because it declares `requires`. `TextConcat` and `TextGet`
are collecting opcodes and contribute `MAY_COLLECT`; typed source lowering still
has no executor or suspending opcode. Assertions, deferred blocks, and scoped
disposal lower into direct lexical CFG.

Supported source contracts also lower directly. Every ordinary closed-world
call evaluates all arguments and inout reads before it checks `requires` at the
call-expression span, then targets the callee's assumed body. A root with
preconditions uses a same-signature checked wrapper; the callable ABI never
carries a dynamic caller span. An inherent receiver invariant executes at body
entry. `old` values are entry SSA values, while exit contracts read the current
receiver writeback and logical result. Normal tails and explicit returns expand
their cleanup suffix before checking the receiver invariant and `ensures`.
Contract predicates cover typed constants, values, bindings, fields, unary and
checked numeric operations, short-circuit Boolean CFG, `is_finite`, and bounded
exhaustive match DAGs. Managed Text leaves remain live through ordinary typed
SSA and exact root-state analysis; contracts do not construct a universal
value or enter an executor.

The lowerer maintains a compiler-only cleanup list while translating one
lexical block. It registers a `defer` when its statement is reached and a
`scoped` value only after its initializer has completed and the local has been
bound. Normal block completion, explicit return, and every fault target expand
the active suffix newest-first. Expansion exposes only older actions while an
action is lowered, so a cleanup fault preserves an existing primary fault,
suppresses later cleanup faults, and still executes every older action. Branch
bodies are independent lexical scopes; they cannot leak registrations into a
join. At most 1,024 actions may be simultaneously active in a function and at
most 65,536 action expansions may be materialized. Exceeding either bound is a
stable `ProgramTooLarge` error rather than fallback.

Scoped concept disposal is closed through the already selected concrete
witness method and uses the ordinary direct or fallible typed call ABI. A
mutable receiver is written back on both normal and unwind edges. Canonical
File and Socket disposal uses the `ResourceClose` terminator: it carries one
exact nominal resource value, returns `Unit` plus the closed resource on its
normal edge, and returns the resource writeback on its fault edge. LLVM calls
the typed close ABI directly; it does not construct a universal `Value`, a
runtime cleanup stack, or an executor. MIR rejects suspension in cleanup, and
LCIR independently rejects a suspending exact callee or an invented suspension
effect in the resulting cleanup graph.

When any reachable instance contains `TextConcat`, `TextGet`, a TextMap, or a
tuple/record/closed-sum containing Text, representation planning selects
`ManagedPointer` for every `Text` in the artifact. `TextLiteral` continues to
produce process-lifetime static objects,
but their pointers share the same callable ABI as dynamically allocated Text.
`TextConcat` returns one managed leaf and has no source fault edge: allocation
resource exhaustion is an uncatchable process fault, while malformed runtime
status fails closed. `TextGet` maps the typed helper's missing/found status to a
zero-initialized `Option[Text]` carrier and traps on any other runtime status.
Products and closed sums remain unboxed exact SSA aggregates. Concrete closed
Lists and TextMaps use direct managed pointers and exact repeated descriptors.
Text inside transparent/refined carriers and other dynamic Text producers still
select whole-artifact fallback.

`plan_managed_roots` computes exact managed SSA liveness with a predecessor
worklist. It records the live-after set at each collecting instruction or call,
excluding the result that does not exist until the call returns. Each live
direct pointer has one empty projection; each live product expands by stable
depth-first field order to `(ValueId, projection)` slots for all managed leaves.
Explicit edge arguments are retained only when their corresponding successor
parameter is live; implicit result and unwind parameters are definitions, not
incoming roots. A sum leaf candidate also records every enclosing variant and
is published only while all corresponding tags are active. Reload reconstructs
only the active payload, including product fields nested inside it. Slot order
is deterministic by value and lexicographic projection, bitmap row zero is
empty, and identical rows are deduplicated. A function with no managed leaf
live across a safepoint has no typed shadow frame. The runtime ABI limits are
checked during LLVM emission; an excess is `ProgramTooLarge`, never legacy
fallback.

`ListConstruct`, immutable `ListAppend`, `ListLength`, and `ListGet` are
first-class typed instructions. Allocation sites root even otherwise-dead List
and managed-element operands before calling `typed-repeated-v1`, then reload
them before copying. The checked-MIR-only `ListAppendUnique` consumes a
greatest-fixed-point `Unique` ownership fact across CFG edges and loop phis;
entry values, copies, calls, aggregate embedding, projections, and ambiguous
joins are `Shared`. Raw LCIR builders cannot forge this certificate.

`TextMapConstruct`, immutable `TextMapInsert`, `TextMapLength`, and `TextMapGet`
are likewise first-class typed instructions. The semantic value argument is
part of the concrete map type, and `get` must return the exact canonical
`Option[V]`; validation independently rejects an erased value or mismatched
option. Construct uses the null empty representation. Insert always performs a
functional copy in this slice, so aliases keep their previous logical value;
future in-place update requires a checked-MIR-only uniqueness proof rather than
an address or reference-count observation. The collecting insertion roots and
reloads the old map, Text key, and every managed leaf of `V`. Length and lookup
do not allocate. The compiler emits no universal map value, runtime type tag,
executor, or global layout registry.

## Typed projected places

Lowering turns each admitted MIR `Place` into a `PlacePlan`. The plan records
the root local, root and leaf `ValueTypeId`/`ReprId` pairs, and the exact
semantic and physical identity of every parent and field step. It contains no
address, executor value, universal `Value`, or runtime callback. Independent
LCIR validation still checks the resulting ordinary product instructions and
their exact types.

`Copy` and `Move` read a projected leaf with a forward `ProductExtract` chain.
A projected `Move` also consumes the complete MIR root; Loom does not create a
partially initialized aggregate. Assignment extracts the required parents and
rebuilds them in reverse with `ProductInsert`. The reconstruction always begins
from the latest root in the SSA environment, not the snapshot used to evaluate
an earlier receiver. A later argument may therefore update a disjoint sibling
without that update being overwritten when the receiver writeback returns.

Projected inout evaluation extracts the receiver at its source argument
position. An infallible call returns the leaf writeback directly. A fallible
call gives both its normal block and a dedicated fault bridge the same typed
leaf writeback; each edge reconstructs the complete root before continuing or
requesting the enclosing fault target. This ordering keeps the SSA environment
ready for lexical cleanup observation, including receiver writeback performed
by a scoped disposer.

Lowering constructs canonical SSA directly: a single continuing branch does
not gain a join, values already dominating every predecessor do not gain
identity block parameters, short-circuit skip edges reuse the evaluated left
operand, and a range header carries only locals written or moved on a
continuing body path. These are generic control-flow/dataflow rules rather than
cleanup left for a later LLVM optimizer.

Per-function SSA environments are persistent sparse radix roots. Branches
share their entry root, local writes copy one bounded path, and joins compare
only subtries that differ from the shared entry. Range headers start from the
same environment and inspect only the body's continuing mutation set. This
keeps lowering proportional to emitted control flow and changed locals instead
of multiplying every branch or loop by the number of live locals.

Range induction uses the reusable `IntSuccessorBelow` instruction. Its operands
carry the exact `current < end` comparison result and upper bound. Independent
validation requires the comparison's true edge to dominate the instruction,
which proves `current + 1` is representable for any signed `Int` upper bound.
LLVM then emits `add nsw` without an overflow edge. The validator and emitter
do not recognize a for-loop, Fibonacci, or another exact MIR shape.

The source root boundary and LCIR artifact boundary intentionally differ. A
run root has no value, type, witness, or receiver inputs and returns `Unit`. A
source test root has no inputs and returns `Unit` or `Result[Unit, E]`.
Eligible closed `Result` instantiations carry an explicit checked outcome plan
into the artifact and native harness. `Err` is a normal failed-test outcome;
it is not a `RuntimeFault`. Unsupported error payloads still select atomic
fallback.

A function contains:

- an `InstanceId`, stable name, source MIR function origin, signature, and
  `Effects` value;
- explicit basic blocks with typed block parameters;
- a dense instruction table and typed SSA values;
- exactly one terminator per completed block.

`InstancePlan` is the single source of callable identity. It is a dense,
deterministic table from each `InstanceId` to an `InstanceKey`. A key contains
the source MIR `FunctionId`, ordered type arguments, and ordered witness
arguments; witness arguments distinguish concrete witnesses, witness
parameters, and owned nested applications. A `Function` stores its
`InstanceId`, not a duplicate key. Its source function in `Origin` is retained
only as provenance, and validation requires it to equal the key's source.
Roots, declarations, direct calls, invokes, and effect analysis consequently
refer to planned instances rather than rebuilding a bare
`FunctionId -> InstanceId` map.

The source lowerer starts from monomorphic exported run or test roots and
computes a bounded closure of executable direct, inherent, and concrete static
concept calls. Each
reachable body is keyed by its source `FunctionId`, exact substituted type
arguments, and the complete static witness-argument tree. Duplicate calls and
different test roots reuse the same key. Exact self and mutual recursion reuse
the already planned instance; a recursive edge that reaches the same source
function with a different key is nonregular and selects whole-artifact
`Unsupported`. Generic declarations outside the selected closure do not affect
route selection.

Planning is iterative and deterministic. It admits at most 4,096 concrete
instances and 16,384 reachable direct-call edges, while each key retains the
shared 256-node combined type-and-witness budget. A call reserves its remaining
edge budget and bounds the fully substituted key before publication. Static
dispatch structurally unifies the selected witness head with its checked
concrete dispatch type, then appends conformance type arguments, conditional
prerequisites, method type arguments, and method proofs in the method
function's declared order. A projection through a function witness parameter
normalizes from that same proof to the witness's concrete associated binding.
An unresolved parameter, unresolved proof, nonregular recursive expansion, or
exhausted planning budget selects one atomic unsupported result before an LCIR
builder exists. Completed keys are
ordered by source function and canonical key identity, so discovery order,
duplicate roots, and repeated compilation do not perturb the artifact.

The resulting LCIR functions and LLVM calls use the instantiated direct
signature. Compile-time witness arguments remain in `InstanceKey` and artifact
identity but consume no runtime argument. Static concept-method dispatch is an
ordinary direct call after closure, and associated projections do not survive
in a completed key or physical representation. Dynamic dispatch still selects
complete legacy lowering rather than introducing a universal value or witness
ABI into this slice.

One public `INSTANCE_KEY_STRUCTURE_BUDGET` limits the combined nested type and
witness structure of a key to 256 nodes. Builders report
`InstanceKeyStructureBudget` before admitting an oversized key, and the
independent validator reports `LcirInstanceKeyStructureBudget` for malformed
unchecked input. Builders report `OpenInstanceKey`, and validation reports
`LcirOpenInstanceKey`, if an unresolved type projection, type parameter, or
witness parameter reaches an LCIR instance boundary. Structure validation,
canonical key encoding, and text output
use bounded iterative traversal instead of recursive descent. The validator
also checks the plan's program brand, dense order, one-to-one length with the
function table, key uniqueness, source-provenance agreement, and every callable
reference.

`BlockId`, `InstructionId`, and `ValueId` are local to one `InstanceId` and
carry that owner in their identity. Entry block parameters correspond to
function parameters. Other block parameters carry values across CFG edges.
All global IDs also carry a private, generative program identity. IDs printed
as the same `i0`, `t0`, or `r0` in separately built programs are not equal and
cannot be used across builders; the private identity is omitted from dumps and
diagnostics so textual output remains reproducible.

The current instruction set is deliberately small:

- `Unit`, `Bool`, `Int`, and bit-exact `Float` constants;
- Boolean negation and equality comparisons;
- floating-point negation;
- floating-point add, subtract, multiply, and divide;
- signed integer comparisons;
- a proof-carrying signed successor below an `Int` upper bound;
- explicitly ordered or unordered floating-point comparisons;
- typed Text literal, concat, Unicode-scalar get, length, containment, and
  content comparison operations;
- closed integer/float parsing, managed float formatting, and Duration
  construction/extraction through existing scalar, sum, product, and fault
  shapes;
- ordinary and invariant-proven product construction, field extraction,
  immutable field insertion, and checked-MIR-only transient protected-receiver
  insertion before an exit invariant check;
- closed-sum construction and exhaustive switching, including managed Text
  leaves guarded by active variants;
- proven refinement and exact unrefinement across one registered transparent boundary;
- direct calls to infallible typed functions.

The current terminators include jump, conditional branch, return, terminal
fault, checked integer negate/add/subtract/multiply/divide, assertion,
fallible `invoke`, typed File/Socket `resource.close`, and `resume_fault`. A
checked operation or invoke has a
`ResultTarget`: the source result exists only on the normal edge, followed by
ordered inout writebacks and separately forwarded arguments. An invoke's
`UnwindTarget` carries only its inout writebacks before forwarded arguments and
is entered with the source fault active. Checked scalar operations have one
normal result and no fault result. This shape makes it impossible to use an
operation result on its fault edge while preserving partial receiver mutation.

Fault state is part of CFG validity. Entry is inactive; ordinary and result
edges preserve their source state; unwind edges make the destination active.
An active path cannot return or originate another terminal fault and must end
in `resume_fault`. Fallible cleanup is still allowed while active. A successful
cleanup operation preserves the primary fault on its normal edge; a later
cleanup fault is suppressed, leaves the first fault primary, and continues on
an active unwind edge so remaining cleanup can run. This is the LCIR form of
the language's deterministic cleanup policy, not a choice left to LLVM.

Managed values outside the admitted Text, List, and TextMap graphs, open or
recursive enums, generic or unsupported-shape runtime
construction and proof replay, dynamic dispatch, contracts over unsupported
value shapes, and coroutine control flow are not implemented. Nongeneric
refined and invariant runtime construction is direct typed CFG returning the
exact `Result[..., ConstraintError]`; portable nongeneric proof replay uses a
canonical runtime-fault assertion before nominal publication. The current CFG
represents direct products, concrete closed sums, both direct Text modes, and
the scalar operations and fault-state transitions which later slices use.

`Origin` records a source MIR function, optional MIR expression, and source
span for each function, instruction, and terminator. There is no inlining
provenance model yet.

## Validation boundary

Fresh checked MIR carries the frontend's process-local
`ConstructionMode::Proven` conclusion for a predicate or record invariant
already established during semantic analysis.
The public raw LCIR builder rejects `RefineProven` and
`InvariantRecordProven`; only the crate-private checked-MIR lowerer can append
them. LCIR deliberately does not encode or re-evaluate the arbitrary source
predicate. Its independent validator checks the certificate's structural
boundary: exact base/result types, protected construction kind, protection on
every representation alternative, representation identity, and the usual SSA
rules. Thus `CheckedProgram` certifies valid LCIR structure while trusting that
fresh frontend conclusion for predicate truth. Portable MIR decoding replaces
it with `Recheck`. For supported nongeneric shapes the lowerer reconstructs the
typed predicate CFG and emits an explicit runtime-fault guard before the
crate-private established-value instruction. The raw builder still cannot mint
that instruction, and a rejected path has no nominal SSA value. Unsupported
generic or value shapes select the complete checking legacy route.

The validator reports independently discoverable `ValidationErrors`; it does
not repair a malformed program. Current checks include:

- canonical registrations, representation tables, well-founded and
  structurally bounded mixed product/sum graphs, canonical sum tags, and dense identities;
- a branded, dense, unique, structurally bounded instance plan whose entries
  agree with function origins and all callable references;
- valid function, block, instruction, value, and value-type references;
- entry parameters matching the function signature;
- no CFG predecessor for the entry block;
- one terminator per block and a valid instruction schedule;
- instruction result shapes and operand types;
- direct-call and invoke arity, types, result types, and exact callee effects;
- edge argument arity and types;
- ordered exhaustive sum cases, exact construction payloads, and typed implicit
  payload parameters on every `SumSwitch` edge;
- one artifact-wide 64-bit `Text` registration, either `ImmortalText` for an
  allocation-free, aggregate-free graph or `ManagedPointer` when concat/get or
  a Text-bearing product/sum or TextMap is present; literal budgets, concat/get
  operand/result types, canonical `Option[Text]` shape, collection effects, and immortal
  literal/closed-flow provenance where that narrower representation applies;
- exact concrete closed `List[T]` and compiler-private `TextMap[V]`
  registrations, including repeated-storage pointer leaves, matching operation
  operands, canonical `Option[T]`/`Option[V]` results, and allocation effects;
- implicit result/writeback parameter shape and type on normal and fault edges;
- exact nominal one-handle File/Socket resource shape, typed close
  result/writeback edges, and required runtime/fault capabilities;
- return types and operation-specific fault-effect requirements;
- the exact minimal transitive effect closure across the complete call graph,
  including capability implications and active-cleanup fault masking;
- no suspending exact callee in a synchronous cleanup graph and no invented
  suspension capability without an LCIR suspension operation;
- consistent inactive or active fault state at every block, including
  `resume_fault` and terminal-boundary rules;
- function ownership for local identities and source origins;
- no duplicate successor from one terminator, except the two logical arms of a
  conditional branch may select one destination;
- no `Uninhabited` signature or SSA value;
- reachable blocks, dominance, and use-after-definition rules.

Aggregate-use validation borrows the canonical representation catalog. A
product construction compares directly against its field slice, and a sum use
selects only its referenced variant. Validation therefore does not clone all
fields or variants for every use; its allocation cost remains bounded by the
program and CFG being checked rather than schema width multiplied by use
count.

When both branch arms carry the same arguments, LLVM emission collapses them to
one unconditional edge. When their arguments differ, the emitter creates two
physical edge blocks so each phi input has a unique LLVM predecessor. Ordinary
distinct-target branches remain direct.

These checks apply both to explicit clients and to the whole-artifact typed
lowerer. The production automatic route consumes only the resulting checked
artifact when the complete reachable graph is supported. Supported source
contracts use the same validated metadata and control flow as explicit LCIR
clients. Assertions keep their exact source span; preconditions keep their
contract span plus the concrete closed-world call-expression blame span. Their
fault edges traverse the same lexical cleanup suffix as any other fault.
Contracts over an unsupported representation or operation still select one
atomic legacy artifact rather than mixing the two native routes.

`ContractFaultMetadata` distinguishes assertion, precondition, postcondition,
and invariant faults. Named contracts carry their source code and the derived
message ``contract `<code>` was not satisfied``; assertions carry no user code
and use `assertion was not satisfied`. Preconditions may use a distinct,
concrete call-site blame span. Postconditions, invariants, and assertions must
blame their contract/assertion span. Independent validation rejects forged
relationships, noncanonical messages, inverted spans, and any user-code or
message field above the public compiler-private 4 KiB UTF-8 budget before a
dump or backend can encode it.

## Text dump

`dump_program`, `write_program`, and `write_program_with_options` traverse a
`CheckedProgram`'s dense tables in their stored insertion order. Repeatedly
dumping the same `CheckedProgram` with the same options produces identical
text. Origins are omitted by default and can be included explicitly.

The dump is not canonical across independently constructed programs. Changing
function, block, parameter, or instruction insertion order may change IDs and
text even when the graphs are otherwise equivalent. The `lcir 19` text includes
canonical representation registrations, the dense instance plan, complete
instance keys including their contract-boundary role, every function's
selected entry block and ordered effect set,
typed runtime/contract fault identity including proof-replay and Duration
guards, closed parse operations, and managed Float formatting,
managed-pointer representations and
`text.concat`, `text.get`, typed resource-close edges, transient
protected-receiver updates, and the checked value type of every block parameter
and instruction result. Representation semantic
types and instance-key arguments use the same complete, iterative type
encoder; no type is represented by a catch-all placeholder. It is
compiler-private and has no compatibility or serialization guarantee.

## Repository evidence

The crate's focused tests cover source-root selection, recursive graph closure,
stable source-graph serialization and errors, branded artifact roots and root
signatures, distinct type/witness instance keys, dense-plan and
instance structural-budget validation, artifact identity and invalidation
inputs, the direct representation catalog, aggregate and match-planner budgets and
large-catalog lookup behavior, target pointer-width validation, block-parameter
joins, loop backedges, pure scalar operations,
infallible direct calls, fallible invokes, edge-defined checked results, active
cleanup paths, lexical LIFO expansion, primary-fault preservation, exact
assertion metadata, typed scoped resource writeback, cleanup depth limits,
recursive effect closure, stable fallible dumps, optional
origins, malformed SSA programs, and source-to-MIR-to-LCIR classification and
dumps for structurally different recursive and iterative Fibonacci programs,
plus zero-cost proven refinements and invariant records. Generic regressions
cover exact regular recursion, duplicate-instance elimination, cross-test-root
reuse, witness-bearing identity, nonregular recursion, bounded key expansion,
unreachable declarations, repeatable dumps and identities, and direct host and
MSVC LLVM signatures. Text regressions cover bounded literal planning,
representation rejection on 32-bit layouts, exact direct calls and generic
identity flow, content comparison, dynamic concat and Unicode-scalar selection,
canonical managed `Option[Text]`, exact live-after root maps,
linear worklist convergence on a large loop, deterministic nested-product leaf
projections, phis, calls, dead edges, forced relocation and alias rebuilds,
cleanup-crossing returns under forced relocation, pointer-free product frame
omission, host execution, Linux/MSVC object
emission, and atomic fallback for unsupported dynamic Text producers or
transparent/refined managed carriers.
Malformed-LCIR tests
prove that ordinary products cannot forge an invariant and that refinement
cannot accept a merely layout-compatible, non-base value.
Structural regressions cover thousands of live locals and identity branches,
bounded persistent-map allocation, and sparse-map reference differentials.
LLVM-side tests additionally cover typed ABIs, block insertion order independent
of dominance order, same-target edge normalization, exact scalar predicates,
checked arithmetic, proved successors, first-primary fault suppression, fatal
runtime setup failures, ordered tests, atomic automatic/legacy route selection,
direct-product construction and mutation, closed-sum construction and ordered
exhaustive matches, tagless/tag-only/tagged ABIs, unusual carrier alignment,
`Result` test outcomes, normal and fault writebacks,
source/interpreter/legacy differentials, an explicit checked-MIR float-pattern
differential across the interpreter and both native routes, shared typed arm
blocks for wide enums, high-use validation against wide schemas, live
optimized sum-carrier SSA, route-separated identity, object-cache
behavior, linking, execution, and verifier/optimization gates on Linux and
macOS. The parameter-driven cross-language benchmark remains on the atomic
legacy route because its root also reaches dynamic text, List, parsing, and
matching;
the direct aggregate tests are the current closed-workload evidence. The
platform-independent Windows CI job checks, lints, tests, and builds
`loom-codegen-ir`; cross-target LLVM tests also emit direct closed-sum MSVC
COFF objects from the same live carrier fixture without selecting the legacy
route.
