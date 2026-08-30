# Code generation IR

`loom-codegen-ir` owns two code-generation boundaries. Its source-graph module
selects checked-MIR function roots and computes the closed-world source graph
used by production native compilation. Separately, its LCIR foundation
provides target-aware scalar, direct Text, closed-product, closed-sum,
transparent nominal, managed Bytes and List, one-field typed Path,
compiler-private typed TextMap and typed Task-handle representations,
canonical structured logging,
compiler-private finite dynamic catalogs, whole-artifact checked-MIR lowering,
typed SSA data structures and builders, independent program and artifact-root
validators, and a textual dump for tests and review.

`loom-codegen-llvm` consumes the resulting `CheckedArtifact` directly and emits
its typed functions and run/test harness without the universal value ABI.
Synchronous artifacts do not construct an executor; a typed async root owns one
executor for its Task lifecycle. Its production prepared router attempts that whole-artifact
lowering once. `Complete` selects only typed LCIR; only `Unsupported` stores a
source reachability graph and selects the complete checked-MIR emitter. Both routes
have independent object identities. The remaining LCIR coverage and deletion
gates are in the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md).

LCIR is compiler-private and target-specific. It is not a source IR, a public
artifact format, or a stable native ABI.

Every LCIR program also carries a `CanonicalTypeCatalog` copied from the
checked MIR prelude after closure remapping. Standard-library types therefore
retain their actual source `TypeId` values; LCIR assigns no fixed numeric slots
to `Result`, `Option`, `TaskFault`, `Bytes`, `Path`, `TextMap`, I/O types, JSON
types, or logging types. Catalog entries are optional for focused programs,
pairwise distinct when present, and required only when the corresponding
representation or opcode is used. The independent validator checks the exact
catalog identity and physical shape together. The complete catalog, including
absent entries, is part of the textual dump and artifact identity.

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
| canonical `Bytes` on a 64-bit target | `ManagedPointer`, one opaque pointer to immutable Text-backed or standalone byte storage |
| canonical `Path` on a 64-bit target | invariant-protected `Product(Text)`, one field using the artifact's canonical Text representation |
| structural tuple | `Product(element value types...)` |
| closed invariant-free record | `Product(field value types...)` |
| closed record with a proven invariant | protected `Product(field value types...)` |
| established monomorphic refined type | its base `ReprId`, with a distinct nominal `ValueTypeId` |
| one-variant closed enum | tagless `Sum(variant payload...)` |
| multi-variant closed enum with no payload fields | minimal integer tag |
| other closed concrete enum | `{ minimal integer tag, exact aligned payload carrier }` |
| concrete closed `List[T]` on a 64-bit target | `ManagedPointer`, one opaque pointer to typed repeated storage |
| concrete closed `TextMap[V]` on a 64-bit target | `ManagedPointer`, one opaque pointer to typed repeated entry storage |
| canonical `File` or `Socket` | opaque `Product(Int)` containing a process-monotonic runtime capability token whose authority is limited to the active Task ledger, never a raw OS handle |
| concrete `Task[T]` in the checked async slice | `TaskHandle`, one stable scheduler-owned opaque pointer excluded from moving-GC maps |
| `dyn C` with one exact artifact-reachable witness | the witness's concrete value representation |
| `dyn C` with a finite closed set of two or more exact witnesses | `ManagedPointer`, one opaque pointer to a candidate-specific tagged box |

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
Runtime-checked constructions, general by-value recursive sums, operations that
split or rebuild Task-bearing products, incomplete dynamic witness sets, and
uninhabited fields are not selected. A concrete List or TextMap
breaks by-value aggregate recursion and may contain any registered closed
direct scalar, Text, task-free product, task-free sum, List, or TextMap value.
Task-bearing elements remain outside this recursive container slice. The
canonical
recursive `Json` sum is admitted through exactly those two indirections:
`Null`, `Bool(Bool)`, `Number(Float)`, `Text(Text)`, `Array(List[Json])`, and
`Object(TextMap[Json])`. No recursive direct payload is admitted, and no
universal value or runtime type registry is introduced.
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
unsupported coverage and selects the complete checked-MIR route. Independent LCIR
validation repeats both limits before LLVM constructs any constant object.

Canonical `Bytes` has one tagless managed-pointer representation. The exact
`Text.encode_utf8` instruction preserves the immutable Text object pointer, so
encoding allocates nothing and the resulting Bytes retains a Text descriptor.
Bytes created by append instead use a distinct ByteObject descriptor with no
Unicode-scalar claim. Both proven descriptor forms share the checked prefix and
trailing-byte layout; no raw pointer, foreign descriptor, or user nominal type
can enter this representation.

`BytesLength`, `BytesGet`, and `BytesCompare` inspect immutable headers and byte
ranges without a moving-GC safepoint. Checked indexing widens the selected
unsigned byte to `Int` and returns the canonical `Option[Int]`; a negative or
out-of-range index returns `None`. Equality is content equality rather than
managed-pointer identity. `BytesAppend` and `BytesDecodeUtf8` are explicit
`MAY_COLLECT` instructions. Append returns a fresh ByteObject. Decode returns
the shared pointer for a valid Text-backed value, allocates a canonical Text for
a valid standalone ByteObject, and otherwise constructs the exact
`Result[Text, DecodeTextError]` invalid-UTF-8 variant. The root plan protects
live inputs across either typed boundary. Each runtime helper publishes its
fully initialized pointer last through a stable output cell. Append may reuse
the result's direct root cell when one exists; decode uses a stable temporary,
then constructs and publishes the exact Result without an intervening
safepoint. These five existing source APIs introduce no JSON-specific operation
or runtime type registry, and require no ownership or borrow syntax.

`TextFromUtf8Units` accepts only the canonical direct `List[Int]` managed
representation and returns the exact closed
`Result[Text, DecodeTextError]`. It is an explicit `MAY_COLLECT` safepoint.
Generated code branches around the null empty-List representation, passes a
borrowed contiguous `i64` view to the format-neutral typed runtime helper, and
constructs the exact Result after the helper returns. Runtime status `0`
selects `Ok`; `-1` selects `InvalidUtf8`; every positive or unknown status
traps as a compiler/runtime defect. The input needs a root only when it is live
after the safepoint because the helper stages all units before allocation.

Canonical Path is one exact invariant-protected product containing the
canonical Text value. Its physical ABI remains the same unboxed product, while
checked MIR rejects raw construction/projection and LCIR rejects ordinary
product construction/insertion for this semantic type.
`PathFromText` scans the immutable UTF-8 bytes for U+0000 and constructs the
exact `Result[Path, PathError]`; `PathAsText` extracts the existing field.
Neither instruction allocates or contributes `MAY_COLLECT`. `PathJoin` takes
two exact Path products, returns the exact Result, and is an explicit
`MAY_COLLECT` safepoint. Status `0` from the typed helper selects `Ok`, `-1`
selects `PathError.AbsoluteJoin`, and every other returned status traps as a
compiler/runtime ABI defect. Its result is one newly allocated Text wrapped in
the one-field product. Root planning protects only inputs and aliases live
after the safepoint because the helper stages both complete Text payloads before
allocation. No runtime Path object, filesystem policy, JSON operation, or
ownership syntax enters LCIR.

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

A concrete `List[T]` or `TextMap[V]` first compares lengths, then walks
equal-length inputs by an Int index. List iterations use nonallocating `ListGet`
operations and compare canonical `Option[T]` results. TextMap iterations read
canonical sorted entries through the compiler-private nonallocating indexed
operation and compare exact `Option[(Text, V)]` results. The loop backedge uses
`IntSuccessorBelow` with the exact `index < length` true-edge proof, so it adds
neither a checked-overflow fault nor a runtime helper. Reads create no
alias-visible mutation and cross no collection safepoint; List and TextMap value
semantics and typed GC roots are unchanged.

Planning admits at most 4,096 concrete equality-helper types and registers
every implicit `Option[T]` or `Option[(Text, V)]` before LCIR construction.
Each compiler-generated `StructuralEquality` instance has the exact signature
`(T, T) -> Bool`, no effects, no coroutine plan, and expands one representation
layer. Non-leaf children use ordinary direct calls to their own exact helpers.
A nominal type reached again through List or TextMap therefore closes a finite
call-graph cycle instead of cloning an unbounded CFG. Helpers are planned only
from reachable equality sites, participate in normal artifact reachability and
DCE, and require no universal comparison instruction or runtime type switch.

The recursive `Json` carrier therefore receives ordinary structural equality
through the same generated List/TextMap helper cycles as user-defined recursive
types. `std.json.parse_json` is an ordinary source call graph over Bytes, List,
Text, `TextMapConstructEntries`, and Float primitives; LCIR contains no JSON
parse instruction.

## Typed JSON formatting

The compiler-known `format_json` operation lowers to one `JsonFormat`
instruction only when its input is the canonical recursive `Json` type and its
result is the canonical `Result[Text, JsonError]`. The instruction records the
checked `Ok`, `Err`, `DepthLimit`, and `NonFiniteNumber` variant identities;
independent validation rederives those relationships and rejects a raw builder
which substitutes a layout-compatible sum.

The instruction is a collection safepoint because a successful result owns a
new managed Text. Exact live-after root planning protects only the Json leaves
and other values which remain live across that point. The runtime first walks
the complete direct Json carrier through the compiler-supplied
`LoomTypedJsonLayout`, writes canonical compact bytes into non-GC staging
storage, and only then allocates and publishes the Text object. Object fields
follow the TextMap's canonical key order. Invalid recursion depth or a
non-finite number returns an ordinary error selector without publishing a
partial Text or activating source fault state.

LLVM consumes the runtime status and constructs the exact direct Result sum in
SSA. The boundary contains no universal `ValueSlot`, source type identifier,
runtime layout registry, scheduler, executor, or source-visible address. JSON
parsing and recursive Json equality use ordinary source and generated-helper
paths, respectively.

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
coverage and selects the atomic checked-MIR route; it is not a lowering defect and
cannot consume the compiler's call stack. Independent LCIR validation enforces
the same limits for explicit builder clients.

Projected-place preflight is independently bounded. One place may cross at
most 64 record fields, and the complete artifact may request at most 65,536
units of aggregate extraction/reconstruction work. Reads and moves charge one
unit per field, writes charge the forward extraction plus reverse insertion,
and inout calls reserve reconstruction on both normal and fault edges. An
invalid field path, a path through a protected or managed parent, excessive
depth, or exhausted work budget produces `Unsupported(ProjectedPlace)` during
classification. The whole artifact then selects the checked-MIR route before any
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
compiler-private identity for the complete checked artifact. The identity
carries the `typed-lcir-whole-artifact` route tag, artifact kind, ordered run
or test roots, and the canonical LCIR dump with origins enabled. Its payload
therefore includes the target, representation and instance plans, checked
functions and control flow, operations, and complete function, instruction,
and terminator origins.

The dump uses explicit enum spellings and string escaping rather than Rust
`Debug`. Dense numeric IDs are content, but the process-local generative
`ProgramBrand` is deliberately excluded, so independently built artifacts with
the same deterministic numbering and content have the same identity. The
production LCIR fingerprint streams this identity together with backend,
target-machine, optimization, runtime ABI, and debug-source identities. The
authoritative current values for every LCIR, object-cache, checked-MIR, and
runtime boundary are maintained in
[Versioning and compatibility](../project/versioning.md).

`IoTaskCreate` covers the seven closed File and Socket operations. Its explicit
error mode selects either the recoverable `Task[Result[T, IoError]]` shape or
the faulting `Task[T]` shape. A direct File or Socket value contains a monotonic
runtime capability token, never an OS descriptor or handle. Read and write
requests resolve that token against the active Task's unique resource-ledger
entry; the runtime copies borrowed Text bytes and duplicates the concrete
resource before the instruction returns. Open, create, and connect completion
place the concrete RAII owner in the child Task's result ledger before
publishing its token. The compiler-generated callback either constructs the
target-native `Result`, publishes the exact success value, or records the
operation-specific Task fault. No mode uses a universal value envelope.

File and Socket I/O is a typed-LCIR-only native boundary. Classification admits
both public operation families, but production preparation may use them only
when the complete reachable artifact lowers to LCIR. If another reachable site
is unsupported, `Automatic` fails closed instead of handing the I/O graph to
the checked-MIR emitter. Direct checked-MIR identity and emission reject the
same reachable builtins; unreachable I/O does not change either route.

`lower_typed_artifact` accepts a checked MIR program, a source run/test
request, and a target layout. It first selects the exported run root or ordered
test roots, validates their source reachability, then closes exact concrete
function instances before classifying any of them. Classification covers the
entire instance and representation plan before allocating LCIR. It returns
either one complete independently checked
`CheckedArtifact` or one deterministic `SupportReport` for the whole artifact.
Invalid roots, resource limits, source-graph defects, and invalid generated
LCIR are structured `LoweringError` values and never select fallback.

The current lowering coverage includes synchronous scalar, direct `Text`,
one-field direct `Path`, structural tuple, closed-record, concrete closed-enum,
and established refined signatures. Async signatures without explicit mutable parameters and their
suspension frames may also use direct scalar/refined/product/Text shapes and
closed sums whose payload graphs contain those shapes, plus concrete closed
`List[T]` and compiler-private `TextMap[V]` one-pointer carriers, including when
these values are nested or lexical cleanup is active across a suspension. Their
bodies may call synchronous functions with functional inout parameters. These
coroutines preserve
`MAY_FAULT` from checked operations, assertions, state-zero preconditions,
ordinary fallible invokes, await fault propagation, and checked timer
construction. A
source `Result[T, E]`, including a managed-Text result, is an ordinary completed
value; Task `Faulted` and `Cancelled` states remain control outcomes. Async
roots with `requires` use the same typed state-zero check and receive their
declaration span as blame from the root harness. Functions declaring explicit
mutable coroutine parameters still fail closed before LCIR creation. Coverage includes bounded
direct generic calls whose concrete types use those representations. Concrete
static concept calls use the selected witness method directly, including
conditional proof applications and normalized associated bindings. A unique
closed dynamic witness is erased to its concrete type, including in async
parameters, results, and recursively nested admitted frame shapes. A dynamic
View parameter used by mutable dispatch in an async body is a by-value
Task-frame value rather than an inout callable boundary; dispatch updates that
independent copy. Two or more artifact-closed exact witnesses use checked
`dyn.construct` and `dyn.switch` operations backed by one managed pointer and
direct candidate calls. The same managed pointer is an exact coroutine
parameter, result, or suspension-live value and composes recursively inside
records, tuples, closed sums, and Lists; the frame and repeated descriptors
trace it through the ordinary managed-root plan. Finite dynamic flow covers
constants, locals and assignment, tuple construction and immutable `let`
destructuring, blocks and conditionals, short-circuit Boolean operations,
integer ranges, pure scalar operations,
checked integer arithmetic, and direct/readonly-inherent calls including
recursion. Finite checks, integer/float parsing, float formatting, Duration
construction/extraction, and async `Task.sleep` also lower directly. Plain
record construction, whole-value copy and move, nested field read/write,
tuple/record nesting, product block parameters, parameters, returns, and
loop-carried products lower directly to SSA. Compile-time-proven
refined construction, exact unrefinement, and compile-time-proven record
invariants are representation-preserving typed operations. Unknown task-free
nongeneric refined predicates and fully concrete task-free nongeneric or generic
record invariants remain normal typed `Result[..., ConstraintError]`
constructions. Open, affine, or unsupported-shape runtime construction selects
whole-artifact fallback. A
decoded `.loomi` MIR
proof replay (`ConstructionMode::Recheck`) for a task-free refined type or
concrete task-free invariant-record instantiation re-evaluates the embedded predicate in typed
LCIR, raises the canonical `ArtifactProofRejected` runtime fault on rejection,
and creates the established nominal value only in the accepted block. Generic
invariant records first apply the current function-instance substitution and then their
independent definition-parameter substitution to fields and lexical contract
bindings. Unsupported concrete representations or contract shapes remain an
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
select atomic fallback. The same synchronous call ABI is valid inside an async
body. Its normal edge installs the result and writebacks before ordinary
continuation. Its fault bridge installs every writeback before requesting the
coroutine's fault target, so `defer` and `scoped` cleanup observe the callee's
latest mutation rather than the pre-call snapshot.
A dense reverse-call worklist computes the least transitive effect fixed point
in linear time and chooses direct calls versus fallible invokes. The effect
set has independent `MAY_FAULT`, `NEEDS_RUNTIME`, `MAY_COLLECT`,
`NEEDS_EXECUTOR`, and `MAY_SUSPEND` capabilities. Collection implies an active
runtime; suspension implies an executor, which implies an active runtime.
`MAY_FAULT` intentionally implies none of those capabilities because checked
scalar faults use only the local fault context. A synchronous caller gains
`MAY_FAULT` when it executes an unknown callee precondition; the assumed
synchronous body does not gain that effect merely because it declares
`requires`. An async precondition instead contributes `MAY_FAULT` to the child
coroutine's state-zero path. `TaskCreate` does not inherit any child effect.
`TextConcat`, `TextGet`, `TextFromUtf8Units`, process argument selection,
process environment lookup, `BytesAppend`,
`BytesDecodeUtf8`, `PathJoin`, `FloatFormat`, and `JsonFormat` are collecting
opcodes and contribute `MAY_COLLECT`. Process argument count, Path construction,
and Path extraction remain non-collecting. `TaskCreate` contributes
`NEEDS_EXECUTOR`, while the `AwaitTasks` terminator contributes `MAY_FAULT` and
`MAY_SUSPEND`.
`TaskJoin` contributes `NEEDS_EXECUTOR` but does not itself suspend.
`TaskSleep` contributes `MAY_FAULT` and `NEEDS_EXECUTOR`, but neither
`MAY_SUSPEND` nor `MAY_COLLECT`: it constructs a first-class Task and does not
wait for it. Assertions, deferred blocks, and scoped disposal lower into direct
lexical CFG.

Supported source contracts also lower directly. A synchronous closed-world call
evaluates all arguments and inout reads before checking `requires` at the call
expression, then targets the callee's assumed body. A synchronous root with
preconditions uses a same-signature checked wrapper. An async call creates its
child without checking the child's contracts in the parent. The coroutine
checks `requires` in state zero before its body, using the creation-site span
carried in the frame; an async root receives its declaration span from the
harness. No source callable accepts a caller-span parameter, and Task creation
does not become fallible because the child may fault. An inherent receiver
invariant executes at body entry. `old` values are entry SSA values, while exit contracts read the current
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

At `AwaitTasks`, the lowerer snapshots one identical exact live-value row for
the normal, child-fault, and cancellation edges. It expands the currently
active cleanup suffix into both non-normal paths. The child-fault path activates
source fault state and ends in `ResumeFault`; the cancellation path preserves
inactive source fault state and ends in the coroutine-only `TaskCancelled`
terminal. Cleanup remains synchronous on both paths. This is static CFG
duplication under the same cleanup budgets, not a runtime registration stack.

Every scoped disposal closes through the already selected concrete witness
method and uses the ordinary direct or fallible typed call ABI. A mutable
receiver is written back on both normal and unwind edges. The canonical source
File and Socket witnesses call authenticated private close leaves; those leaf
calls lower to `ResourceClose`, which consumes one exact nominal resource value
and produces `Unit` plus the closed resource without raising a source fault.
Independent validation accepts only the cataloged canonical `File` for the File
kind or canonical `Socket` for the Socket kind. Each is the registered direct
one-field product whose sole `Int` is an opaque runtime capability token; it is
never a raw descriptor or handle. An unregistered, generic, structurally
similar, or representation-alternative nominal fails closed. LLVM calls the
typed close ABI directly, and the runtime accepts the token only when the active
Task owns its unique, kind-matching ledger entry. Normal code must be running
and not cancelled; the same exact owner may close during the executor-guarded
cancellation or result-disposal cleanup phase. Other I/O remains forbidden in
that phase. An invalid or already-closed sentinel is rejected rather than
treated as a second successful close. Runtime status `0` produces both
instruction results, and every other status traps as an ABI defect. The path
does not construct a universal `Value`, a runtime cleanup stack, or another
executor. MIR rejects suspension in cleanup, and LCIR independently rejects a
suspending exact callee or an invented suspension effect in the resulting
cleanup graph.

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
checked during LLVM emission; an excess is `ProgramTooLarge` and does not change
route selection.

`ListConstruct`, immutable `ListAppend`, `ListLength`, and `ListGet` are
first-class typed instructions. Allocation sites root even otherwise-dead List
and managed-element operands before calling `typed-repeated-v1`, then reload
them before copying. The checked-MIR-only `ListAppendUnique` consumes a
greatest-fixed-point `Unique` ownership fact across CFG edges and loop phis;
entry values, copies, calls, aggregate embedding, projections, and ambiguous
joins are `Shared`. Raw LCIR builders cannot forge this certificate.

`TextMapConstruct`, `TextMapConstructEntries`, immutable `TextMapInsert`,
`TextMapLength`, `TextMapContains`, `TextMapGet`, checked indexed
`TextMapEntryGet`, and immutable `TextMapRemove` are likewise first-class typed
instructions. The semantic value argument is part of the concrete map type;
`get` must return the exact canonical `Option[V]`, while source `entry_at` must
return the exact canonical `Option[(Text, V)]`. Independent validation requires
canonical managed `Text` keys. Construct uses the null empty representation.
Insert and successful multi-entry removal perform functional copies, so aliases
keep their previous logical value; a missing removal reuses the original
pointer and removing the final entry returns the canonical null value.

`TextMapConstructEntries` accepts the exact canonical `List[(Text, V)]` and
produces the exact canonical `Result[TextMap[V], Text]`. It is one collecting
bulk construction: successful input is sorted once by canonical UTF-8 key order
and published as one map, while duplicate input returns the lexicographically
smallest duplicated Text key. Independent validation rederives the List tuple,
key, value, map, Result, and error Text identities. The instruction is not a
chain of `TextMapInsert` operations and exposes neither mutable TextMap storage
nor ownership syntax.

Insertion roots and reloads the old map, Text key, and every managed leaf of
`V`. Removal locates and consumes its key before allocation, roots exactly the
source map, reloads it after possible relocation, and copies the entry ranges
on either side of the removed position. Length, containment, lookup, and the
checked indexed entry read do not allocate. Structural equality first
compares lengths and then walks the canonical sorted entries as exact
`Option[(Text, V)]` values; it therefore ignores insertion history while
recursively preserving the normal scalar, product, sum, List, and TextMap
equality rules. A nominal cycle reached again through List or TextMap closes
through the corresponding compiler-generated structural-equality helper.
The compiler emits no universal map value, runtime type tag, executor, or
global layout registry.

## Typed stackless coroutines

An admitted async function carries a checked `CoroutinePlan` in addition to its
ordinary LCIR signature and CFG. The plan fixes the output type, whether the
frame carries a creation-site span for state-zero preconditions, and the dense
resume-state sequence `1..n`. Each row records an `AwaitMode` and one ordered
exact output type for every child, followed by, in deterministic MIR-local
order, the exact LCIR types forwarded across that suspension. The child-output
row always retains its full arity: `all` derives one normal result per child,
`any` derives one result of the common child type, `settled` derives every
terminal child handle, and `race` derives the terminal winner handle. Independent
validation matches every row to exactly one mode-identical `AwaitTasks`
terminator, checks all child Tasks, continuation parameters, and forwarded
values, rejects duplicate child handles, and rejects a Task edge without an
active coroutine plan. It also requires each await's normal, fault, and
cancellation edges to carry the identical exact live row; only the normal edge
has leading mode-derived results. `TaskCancelled` is valid only on a checked
cancellation path and no cancellation path may suspend. The canonical dump
includes the complete plan, so it is also an artifact-identity and object-cache
input.

`TaskCreate` constructs a scheduler-owned `Task[T]` for one exact coroutine
instance. The handle is a stable opaque pointer, not a moving object and not a
Promise or universal value. It carries the instruction's source span into a
callee frame only when the checked coroutine plan requires dynamic precondition
blame; this does not copy the callee's fault effects to `TaskCreate`. The hidden
executor is the current checked execution context. A coroutine callback may
forward it through any number of synchronous helpers whose transitive effects
contain `NEEDS_EXECUTOR`; those helpers borrow the pointer and never create or
drive an executor. `AwaitTasks` stores all ordered
children and the row's live values, prepares one structured mode-specific join,
publishes the frame/root state, and exposes explicit normal, child-fault, and
cancellation edges: `normal` is a `ResultTarget`, `fault` is an `UnwindTarget`,
and `cancel` is a `BlockTarget`. Exact mode-derived values exist only on the
normal resume edge; all three edges receive the same exact live row. `all`
injects every exact child result, `any` injects one successful result,
`settled` injects every terminal child handle, and `race` injects only the
terminal winner handle. An ordinary single-child await is the same operation
as one-child `all`.
A join-suspend status of one returns `pending`; zero means the child was already
terminal, so the runtime removes the redundant wake-up, keeps the active parent
`Running`, and enters the same checked result/reload edge in the current
callback. Any other status is a runtime/compiler defect. Ordinary expression
evaluation never creates or runs a second executor; synchronous helpers only
borrow the executor already driving their async caller.

`TaskSleep` is a separate explicit fallible terminator admitted in any checked
executor context. Its input is canonical `Int` milliseconds; a source
`Duration` is normalized first with `ProductExtract`. The normal edge receives
the canonical `Task[Unit]` handle, while the fault edge preserves the source
origin. LLVM rejects a negative duration, checks the signed conversion from
milliseconds to nanoseconds, reads the monotonic clock, checks the unsigned
deadline addition, and then calls
`loom_typed_timer_task_create_v1(executor, deadline_ns)`. Task creation itself
does not suspend; a later `AwaitTasks` does.

An immediately awaited fixed tuple or fixed Task-policy call evaluates children
left to right and lowers directly to `AwaitTasks`, avoiding an intermediate
composite. A first-class stored fixed policy lowers to `TaskJoin`. `all` returns
the exact result tuple, `settled` the corresponding outcome tuple, `any` one
homogeneous winner, and `race` one homogeneous winner outcome. LLVM generates
one exact typed composite frame for that static mode and result shape. Runtime
adoption validates the complete ordered child set before transferring it from
the current parent and publishing the composite; the checked-MIR universal
join-result path is never called.

A nonempty, immediately awaited, fixed-arity `Task.any` also lowers directly to
`AwaitTasks` when every child has the same exact output type. The plan and frame
retain all child entries, but the normal continuation receives only one implicit
winner result. The runtime finalizes and retires losers before the callback
observes the completed step. Generated code then switches on the original winner
ordinal, loads that exact child pointer from its static frame field, and takes
the result. A loser-disposal fault enters the await fault edge with that fault
active; if no child succeeds, generated code raises canonical `TaskAnyFailed`
at the `Task.any` expression origin before static coroutine cleanup. Immediate
fusion therefore preserves the same producer origin as a stored `TaskJoin`.

Immediately awaited fixed `Task.settled` and `Task.race` calls use the same
`AwaitTasks` terminator. Their normal edges receive terminal affine handles,
not hidden universal outcomes. Lowering emits `TaskOutcomeTake` immediately for
each handle and constructs the canonical `TaskOutcome[T]` sum explicitly.
Independent validation requires an exact `Task[T]` handle from the leading
implicit parameter of a dedicated matching `settled` or `race` normal block,
checks the canonical `TaskFault` and `TaskOutcome` shapes, and enforces one
consumption. The instruction is `MAY_COLLECT | NEEDS_EXECUTOR`: completed values
move directly, fault code and message become managed Text, cancelled values have
no payload, and existing live outcomes are rooted across later captures.

On the completed branch, exact outcome extraction transfers the child's
resource-ledger entries, which back any published File or Socket capability
tokens, to the active owner Task before the terminal child is retired. A direct
take from the ownerless root leaves those entries in that root's ledger until
explicit close or executor teardown. Faulted and cancelled outcomes transfer no
entries. Completed loser or unconsumed result disposal closes every concrete
resource left in the child ledger before retired-task memory reclamation,
including when a disposer reports a fault or protocol defect. This is runtime
bookkeeping behind the existing exact typed take instructions, not an LCIR
ownership field or a source ownership operation.

Runtime take preflight independently requires one exact owned/join membership,
a successfully settled join, `TaskResultTake` only for `all` or `any`,
`TaskOutcomeTake` only for `settled` or `race`, and completed winner
finalization for `any` or `race`. Rejection is transactional and cannot mutate
the result cell, join topology, or resource ledger.

A sole nonempty List literal is flattened to the same static child row without
constructing the input List. `all` and `settled` build a fresh result List from
the ordered resumed values; `any` and `race` retain their scalar result. The
frontend currently reaches this path only after semantic resolution has
selected the temporary compiler-private `TaskIntrinsic`; LCIR lowering never
checks the source name. `TaskIntrinsic` is an implementation bridge for API
shapes that the current source type system cannot yet declare. It is not a
standard-library identity, source ABI, or persistent extension point.

Every other exact homogeneous `List[Task[T]]` policy becomes `TaskJoinList`.
The opcode consumes one affine top-level carrier and returns the precise Task
type selected by the policy. The List's element representation is one stable
`TaskHandle`; it has no managed offsets. The composite descriptor instead roots
the source List while child handles are being read and roots the exact output
List while `all` or `settled` captures results. Nonempty construction adopts
the existing contiguous child row directly, without a temporary pointer copy.
Empty `all` and `settled` publish an already-complete empty result; empty `any`
and `race` publish canonical `EmptyTaskJoin`.

The completed source boundary instead resolves each public Task policy to its
ordinary definition `DefId` in the compiler-owned `std` module. Normal
reachability follows that function body, which may call private typed
join/select readiness, exact value-or-outcome extraction, and structured
cancellation-and-drain primitives. Neither semantic analysis nor LCIR maps the
public `DefId` back to a policy enum. The temporary Task catalog and
`TaskIntrinsic` are deleted when the general source-level associated-function
and tuple/List mechanisms can express the API. Runtime-width Lists remain a
distinct typed instruction because their child row is dynamic rather than a
fixed LCIR suspension row.

LLVM derives a target-laid-out frame containing state, parameters, optional
creation-site span coordinates, one ordered child-pointer row plus one
live-value row per suspension, and the typed result.
The coroutine result must use the semantic type's canonical LCIR
representation: `Task[T]` intentionally carries no second hidden layout ID, so
producer and consumer cannot disagree about the result ABI. LLVM emits one
immutable typed-task descriptor with exact managed-leaf byte offsets and a
bitmap for each resume state plus completed-result state. A source coroutine's
generated resume callback is also its descriptor cancel callback. It reads the
cancel-request bit before dispatching by frame state: ordinary state zero enters
the LCIR entry, ordinary nonzero states use the existing join-step ABI, and a
cancel request enters the corresponding checked cancellation state. A normal
`all` join takes every exact child result in source order; a normal `any` join
takes only its selected exact result; `settled` and `race` forward the terminal
handles consumed by `TaskOutcomeTake`. A fault activates source fault state;
cancellation leaves source fault inactive. Every nonzero path reloads the same
exact live row before entering LCIR. Normal return publishes the exact typed
result and completion; cleanup-expanded child-fault and cancel paths end in
`ResumeFault` and `TaskCancelled`, respectively.
The run/test harness creates an executor for the root Task, runs it to a
terminal state, takes the exact result, reports a root fault if one is exposed
by a later slice, and destroys the executor.

The current source boundary is deliberately smaller than the runtime ABI. A
coroutine signature has no functional inout parameters or writeback results,
and a declaration with an explicit mutable coroutine parameter fails closed.
A dynamic View parameter is instead copied by value into the Task frame, so
synchronous mutable dispatch updates only that independent copy. The body may
call a synchronous function with functional inout parameters. Its normal and
fault writebacks update the coroutine's current SSA environment; a fault
writeback is installed before control enters the active static cleanup suffix.

Parameters, results, calls/returns, CFG values, and live frame values admit
direct scalar/refined/product/Text shapes, closed sums, and canonical
one-pointer `List[T]` or compiler-private `TextMap[V]` carriers. Those locations
also admit whole affine Task-bearing products, sums, and proven transparent
wrappers;
their TaskHandle leaves are excluded from moving-GC roots. A unique closed dynamic
witness is recursively physicalized to its concrete representation in those
locations. A finite closed catalog uses the existing exact one-pointer managed
dynamic representation, including when nested in products, sums, Lists, or a
completed Task result. The recursive frame walk consumes one shared bounded
structural budget, so cyclic or non-regular generic expansion fails closed
instead of growing the compiler stack. Dynamic-concept frame producers with
unresolved parameters or projections, raw readiness, and cancellation sources
remain atomic whole-artifact fallback.
Fixed argument joins and runtime-width homogeneous List joins are admitted both
as first-class Tasks and when consumed later by `.await`; `any` and `race`
additionally require one homogeneous output type.
An exact transitive `NEEDS_EXECUTOR` effect adds one compiler-private executor
parameter to a synchronous function after its optional fault context. Direct
calls and invokes forward the caller's current executor in that fixed ABI
order. The helper remains non-suspending: `.await`, terminal outcome taking,
and cancellation dispatch still require a checked coroutine. A synchronous
run or test root may not require the hidden capability; this is validated before
an unsupported LCIR site can select fallback and proves every admitted helper
chain originates at an async-root executor.
The callback forwards child fault/cancel terminal states without turning them
into source `Result` values. Established cancellation remains primary if a
cleanup action faults; the existing runtime suppression rule continues older
cleanup and requires no new ABI.

The LLVM layout planner applies one collision-free rule to every payload-bearing
tagged sum. Target data supplies each payload's size, alignment, and recursive
managed-pointer offsets. Those pointer-width byte ranges form its pointer
class; every remaining payload byte, including padding, is non-pointer. In
stable source order the bounded planner places pointer-free variants first,
then selects the lowest aligned offset at which pointer and non-pointer classes
never overlap. Bytes of the same class may overlap, so compact ordinary enum
layouts are retained. Constructors zero the complete carrier before inserting
the active payload. Pack/unpack, active managed-root publication and rebuild,
and List/TextMap repeated descriptors all consume the same offset plan.

Carrier storage is limited to 64 KiB. All layout plans in one artifact share a
65,536-byte-step placement budget; completing one plan cannot reset the budget
for the next sum. Pack/unpack operations independently share a
65,536-payload-byte budget across the complete emitter, bounding bytewise LLVM
instructions even when every individual carrier layout is representable.
Checked overflow or budget exhaustion is an emission-time `ProgramTooLarge`
failure before an object or partial IR output is written. Independent LCIR
validation remains responsible for semantic sum shape and does not guess
target byte offsets. As one consequence,
canonical `Json` remains 24 bytes on supported 64-bit targets: tag byte 0,
scalar payload byte 8, and managed payload cell byte 16. `List[Json]` therefore
has stride 24 and pointer offset 16, while `TextMap[Json]` entries have stride
32 and pointer offsets 0 and 24. The same rule covers unrelated and nested
closed sums; there is no Json-specific layout branch. Unsupported 32-bit
managed layouts fail closed before LLVM emission.

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
- an optional checked coroutine plan with exact suspension-live types;
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
in a completed key or physical representation. Dynamic instance closure erases
a unique closed proof or retains only the called requirement slot for every
member of a finite closed candidate catalog. Generic and conditional
conformances participate when their concrete types and prerequisite proof trees
are closed. Missing producers and proofs with unresolved parameters or
projections still select complete checked-MIR lowering; no universal value,
runtime registry, or witness ABI enters typed LCIR.

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
- typed Text literal, concat, Unicode-scalar get, length, containment, content
  comparison, and UTF-8-unit construction operations;
- typed process argument count/selection and environment lookup, with exact
  direct-Text results and canonical `Option[Text]` construction;
- typed Bytes operations and exact Path construction, Text extraction, and
  lexical join;
- closed integer/float parsing, managed float formatting, and Duration
  construction/extraction through existing scalar, sum, product, and fault
  shapes;
- ordinary and invariant-proven product construction, field extraction,
  immutable field insertion, and checked-MIR-only transient protected-receiver
  insertion before an exit invariant check;
- closed-sum construction and exhaustive switching, including managed Text
  leaves guarded by active variants;
- proven refinement and exact unrefinement across one registered transparent boundary;
- typed coroutine Task construction;
- typed File/Socket `resource.close`, producing `Unit` and the closed resource;
- direct calls to infallible typed functions.

The current terminators include jump, conditional branch, return, terminal
fault, checked integer negate/add/subtract/multiply/divide, assertion,
fallible `invoke`, `task.await`, and `resume_fault`, plus coroutine-only
`task.cancelled`. A
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
An `AwaitTasks` child-fault edge is an unwind edge and therefore activates this
state. Its cancellation edge preserves inactive source fault state and may end
only in `task.cancelled`; cancellation cleanup cannot create, aggregate, or
await Tasks, including through an executor-dependent callee. An active
source-fault cleanup likewise cannot await again before `resume_fault`.

Managed values outside the admitted Text, List, and TextMap graphs, open or
recursive enums, open, affine, or unsupported-shape runtime construction,
affine or unsupported-shape proof replay, incomplete dynamic witness catalogs, derived
dynamic proof conversion, contracts over unsupported value shapes, and
coroutine forms outside the bounded typed slice are not implemented. Nongeneric
task-free refined and fully concrete task-free invariant-record runtime
construction is direct typed CFG returning the exact
`Result[..., ConstraintError]`; portable task-free refined and concrete
task-free invariant-record proof replay uses a canonical runtime-fault
assertion before nominal publication. The current CFG
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
fresh frontend conclusion for predicate truth. `.loomi` MIR decoding replaces
it with `Recheck`. For supported task-free concrete shapes the lowerer reconstructs the
typed predicate CFG and emits an explicit runtime-fault guard before the
crate-private established-value instruction. The raw builder still cannot mint
that instruction, and a rejected path has no nominal SSA value. Unsupported
generic or value shapes select the complete checked-MIR route.

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
- the exact canonical `Bytes` nominal registration as one `ManagedPointer`,
  including Text-backed encode provenance, operation operand/result types,
  canonical `Option[Int]` and `Result[Text, DecodeTextError]` shapes, and exact
  append/decode collection effects;
- exact concrete closed `List[T]` and compiler-private `TextMap[V]`
  registrations, including repeated-storage pointer leaves, matching operation
  operands, canonical `Option[T]`/`Option[V]` results, and allocation effects;
- canonical concrete `Task[T]` handles, exact coroutine output/frame types,
  dense unique resume states, matching `task.create`/`task.await` edges,
  identical exact live rows on normal/fault/cancel await edges, normal-only
  child results, cancellation-path provenance, continuation arguments, and
  executor/fault/suspension effects; coroutine managed-pointer slots accept
  only canonical direct Text/List values or compiler-private `ManagedTextMap`
  values, while dynamic boxes continue to fail closed;
- implicit result/writeback parameter shape and type on normal and fault edges;
- exact cataloged canonical direct one-`Int` product registration for `File` or
  `Socket`, exact agreement between the nominal type and `ResourceClose`
  kind, its exact Unit/resource result pair, and the required executor
  capability without a source fault capability;
- exact File/Socket operation, argument, success, and Task result shapes for
  every `IoTaskCreate`; recoverable mode requires the canonical
  `Result[T, IoError]`, faulting mode requires `Task[T]`, and both consume the
  same closed runtime error-kind and managed-message outcome domain;
- return types and operation-specific fault-effect requirements;
- the exact minimal transitive effect closure across the complete call graph,
  including capability implications and active-cleanup fault masking;
- no suspending exact callee in a synchronous cleanup graph and no invented
  suspension capability without checked coroutine control flow; cancellation
  cleanup and `task.cancelled` paths remain scheduler-topology neutral, and an
  active source-fault cleanup cannot await again;
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
clients. Assertions keep their exact source span. Synchronous preconditions keep
their contract span plus a static closed-world call-expression blame span;
async preconditions keep the same contract span plus the validated creation-site
span carried in the coroutine frame. Root async Tasks receive their declaration
span from the harness. Their fault edges traverse the same lexical cleanup
suffix as any other fault.
Contracts over an unsupported representation or operation still select one
atomic checked-MIR artifact rather than mixing the two native routes.

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
text even when the graphs are otherwise equivalent. The canonical text includes
canonical representation registrations, the dense instance plan, complete
instance keys including their contract-boundary role, every function's
selected entry block and ordered effect set,
typed coroutine plans, optional carried caller-span metadata, dynamic
precondition blame, and Task control flow, including fallible `task.sleep`,
explicit await modes and normal/fault/cancel targets, `task.outcome_take`, and
`task.cancelled`,
typed runtime/contract fault identity including proof-replay and Duration
guards, the closed Float parse operation, ordinary source-lowered integer
parsing, and managed Float formatting,
managed-pointer representations, finite dynamic candidate catalogs,
`dyn.construct`, `dyn.switch`, mode-qualified `io.task_create.*.result` and
`io.task_create.*.fault`, and
`text.concat`, `text.get`, `text.encode_utf8`, `text.from_utf8_units`, the
typed Bytes operations, `path.from_text`, `path.as_text`, and `path.join`,
`json.format`, typed resource close, structured-log edges, transient
protected-receiver updates, typed TextMap containment/removal/indexed-entry
operations, and the checked value type of every block parameter and instruction
result. Representation semantic
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
Path regressions cover the canonical one-field product, exact closed error
selectors, non-collecting construction/extraction, collecting join effects,
stable dumps, malformed shapes, live aliases through moving-GC pressure, and
production run/test roots without untyped path-helper symbols.
Coroutine regressions cover malformed plan rows, canonical plan identity,
typed Task construction, four ordered root suspension states, a nested
two-state coroutine with a live Task handle and deterministic immediate-ready
second child, scalar/Text/product results, exact managed frame bitmaps, parent
Text relocation while a child allocates beyond the initial 64 KiB collection
threshold, run/test root lifecycle, interpreter/checked-MIR/typed differential
execution, and Linux/MSVC objects. Fallible coroutine regressions additionally
cover managed `Result[Text, E]` completion, exact completed and suspension
carrier offsets/bitmaps, active-tag shadow-root rebuilds, inactive zero lanes,
two-stage forced relocation, checked invokes, assertions, preconditions and
postconditions, exact primary-fault inheritance, sibling
cancellation, and balanced typed callback roots on completed, pending, faulted,
and cancelled exits. Async-cleanup regressions additionally cover exact live
rows on all three await exits, static LIFO cleanup after normal resumption,
child fault, and cancellation, scoped-resource cleanup across suspension,
cancel-request state dispatch, and rejection of suspending or forged
cancellation paths.
Async-writeback regressions additionally cover synchronous functional inout
calls before and after suspension, normal and fault receiver writeback, fault
writeback before lexical cleanup, by-value dynamic View parameters with mutable
dispatch, unique closed dynamic erasure in coroutine parameters, results, and
nested frame shapes, and finite dynamic calls whose values are not
suspension-live.

Typed-I/O regressions cover all seven operations and both error modes, exact
direct Task/result layouts, runtime request/outcome validation, resource-ledger
transfer and cleanup, checked-MIR rejection, unreachable-I/O reachability, and
the rule that an otherwise unsupported reachable I/O artifact may not select
`Automatic` checked-MIR fallback. A dedicated source fixture closes real
`check/build/test/run` commands and inspects its object for only the typed
I/O/task/resource symbols. The integrated standard-library test prepares its
native object through the production router, requires LCIR, and exercises real
filesystem and loopback-socket traffic on the host test platform.

Malformed-LCIR tests prove that ordinary products cannot forge an invariant and
that refinement cannot accept a merely layout-compatible, non-base value.
Structural regressions cover thousands of live locals and identity branches,
bounded persistent-map allocation, and sparse-map reference differentials.
LLVM-side tests additionally cover typed ABIs, block insertion order independent
of dominance order, same-target edge normalization, exact scalar predicates,
checked arithmetic, proved successors, first-primary fault suppression, fatal
runtime setup failures, ordered tests, atomic automatic/checked-MIR route selection,
direct-product construction and mutation, closed-sum construction and ordered
exhaustive matches, tagless/tag-only/tagged ABIs, unusual carrier alignment,
`Result` test outcomes, normal and fault writebacks,
source/interpreter/checked-MIR differentials, an explicit checked-MIR float-pattern
differential across the interpreter and both native routes, shared typed arm
blocks for wide enums, high-use validation against wide schemas, live
optimized sum-carrier SSA, route-separated identity, object-cache
behavior, linking, execution, and verifier/optimization gates on Linux and
macOS. The parameter-driven cross-language benchmark remains on the atomic
checked-MIR route because its root also reaches dynamic text, List, parsing, and
matching;
the direct aggregate tests are the current closed-workload evidence. The
cross-platform release matrix builds `loom-codegen-ir`; cross-target LLVM tests
also emit direct closed-sum MSVC
COFF objects from the same live carrier fixture without selecting the checked-MIR
route.
