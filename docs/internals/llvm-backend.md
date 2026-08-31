# LLVM backend

`loom-codegen-llvm` is the native backend. Its production API prepares one
opaque object from `loom_mir::CheckedProgram` and owned emission options. The
preparation owns the exact LLVM target machine and one complete independently
validated LCIR artifact. Fingerprinting and emission consume that same
preparation. Linking is a separate driver operation.

## LLVM integration

The workspace currently targets LLVM 19 through:

- `inkwell 0.10.0` with `llvm19-1-prefer-dynamic`;
- `llvm-sys 191.1.0` with dynamic linking preferred.

Most compiler crates forbid unsafe Rust. `loom-codegen-llvm` denies it by
default and allows small audited exceptions for raw LLVM C API calls and
Inkwell's typed GEP builder. GEP pointee/index proofs come from independently
validated LCIR layouts. Runtime FFI implementation is isolated in
`loom-runtime`, which explicitly permits the unsafe operations required by the
compiler-private C ABI.

Contributors need LLVM 19 development files and a matching `llvm-config`. The
workspace does not silently fall back to another LLVM major version.

## LCIR foundation status

The workspace contains a direct typed-SSA foundation in `loom-codegen-ir` for
primitive values, literal or concat-produced `Text`, structural tuples,
one-field typed Path, closed records, established transparent refined values,
concrete managed Lists, and compiler-private concrete `TextMap[V]` values,
plus canonical structured logging over direct `Text` and `TextMap[Text]`
values. Its checked coroutine
slice also covers ordered multi-child awaits, nonempty static forms of all four
standard Task composition policies, exact terminal outcomes, and async
state-zero preconditions with creation-site blame.
Tuples and records are recursive acyclic products of other direct values and
may contain one another.
The LCIR emitter accepts only a closed `CheckedArtifact`: its roots, callable
closure, representations, CFG, types, proof-boundary shapes, and exact fault
effects have already crossed independent validation. Predicate truth itself is
a process-local conclusion supplied by fresh checked MIR. Supported `.loomi`
fully instantiated `Recheck` constructions re-evaluate their serialized
predicate or invariant in LCIR and publish the nominal value only on the
accepted path; unsupported replay is a native compilation error. The emitter
declares every source function with its typed LCIR ABI, keeps source symbols
internal, emits a run or ordered test harness, verifies before and after
optimization, and writes a relocatable object.

`build`, `run`, `test`, and `debug` create one target machine and attempt the
complete lowering exactly once. `Complete` retains the checked artifact.
`Unsupported` becomes a structured native-preparation error containing the
ordered `SupportReport`, including stable feature, function, expression, span,
and path facts for each unsupported reachable site. Unsupported unreachable
code cannot affect the artifact; one unsupported reachable test rejects the
whole ordered-test artifact. Invalid programs, invalid roots, resource limits,
compiler defects, and LLVM emitter failures remain distinct hard errors.

File, Socket, and process operations use the same rule as every other native
feature: they must lower completely to typed LCIR. An unreachable private
helper does not affect native preparation.

Source contracts are part of the checked LCIR artifact. LCIR carries canonical
assertion, precondition, postcondition, and invariant fault metadata, including
bounded user code, message, contract span, and either a static blame span or the
validated creation-site span carried by a coroutine frame. Synchronous callers
check `requires` before entering an assumed body. Async Tasks check it in state
zero, so Task construction does not inherit the child's fault effect; the root
harness supplies the declaration span when no creation expression exists. The
LLVM emitter preserves the established contract-channel JSON schema and routes
all contract faults through the active lexical cleanup suffix.

The implemented crate boundary is documented in
[Code generation IR](codegen-ir.md). The accepted pipeline design,
whole-artifact rule and typed ABI are in the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md).

## Prepared object boundary

The production facade consists of:

- `prepare_native_object`, which owns `EmitOptions`, creates the target, and
  constructs one immutable checked LCIR artifact;
- `prepared_native_object_fingerprint`, which hashes the stored artifact without
  repeating lowering or reachability;
- `prepared_native_target_identity`, which exposes the exact read-only target
  identity to runtime-bundle validation;
- `emit_prepared_native_object`, which borrows the same target machine and
  checked artifact.

`PreparedNativeObject` is opaque and remains on the thread that prepared it;
the contained Inkwell target machine is not made artificially sendable. The
low-level typed emitter remains available for focused tests. Production CLI
paths use only the prepared facade.

Preparation failures have six structured classes: invalid program, invalid
root, unsupported feature, resource, target/configuration, and compiler defect.
Unsupported errors and ordinary invalid/resource failures use the failure exit;
target errors use the usage exit and defects use the defect exit.
Classification never depends on matching diagnostic strings. Every unsupported
reachable operation retains its support-report evidence.

## Target-machine policy

For an implicit host target, the backend normalizes the standard target triple
that the compiler itself was built for and uses the actual host CPU
name/features. It does not use LLVM's OS-version-qualified runtime default;
otherwise a macOS bundle could become tied to the packer's Darwin point
version. For any explicit `--target-triple`, including one equal to the host
triple, it uses `generic` CPU, an empty feature set, PIC relocation, and the
target's LLVM data layout.

The target machine is created before representation selection. Its pointer
width is converted with checked arithmetic into `TargetLayout`. A complete
typed LCIR object whose representations are all width-independent can
therefore be emitted for a matching 32-bit LLVM target. Both direct `Text`
representations require 64-bit pointers, so reachable Text makes 32-bit native
preparation unsupported. This does not establish 32-bit runtime, linker, CI, or release support; LLVM
target availability proves only object emission.
A complete width-independent LCIR artifact remains eligible for 32-bit object
emission.

## Verification and optimization

The module triple and data layout are set before lowering. The pipeline is:

1. emit reachable function instances, typed descriptors, runtime declarations,
   and debug metadata;
2. run the LLVM verifier;
3. run the selected pass pipeline;
4. run the verifier again;
5. emit a relocatable object.

The current pass strings are:

| Profile | Pipeline |
| --- | --- |
| development | `default<O0>,globaldce` |
| release | `default<O2>,globaldce` |

Verifier or pass-manager failure is a compiler defect. Optimization must not
change checked overflow, value copy, contract, cleanup, GC, concept, or Task
semantics.

## Runtime requirements

After reachability, a fixed-point analysis classifies each reachable callable's
need to:

- raise a compiler/runtime fault;
- enter a moving-GC collection boundary;
- use the async executor.

These flags are compiler-private lowering facts, not source effects. They
allow a proven pure direct native body to omit status and hidden runtime
context, a synchronous managed root to create only a runtime, and an async root
to attach an executor only when required. A non-coroutine function with
`NEEDS_EXECUTOR` receives one hidden pointer after any hidden fault context.
Direct calls and invokes append the callee's exact hidden arguments in that
order, allowing nested synchronous Task-producing helpers to borrow the one
executor already driving their coroutine caller. They do not create, run, or
destroy an executor, and a synchronous executable root requiring that pointer
is rejected before emission.

These requirements describe execution state, not compiler-generated harness
output. Even a pure direct LCIR executable may declare the output-only
`loom_runtime_stdout_write_v1(data, length)` symbol while constructing no Loom
runtime or executor. The typed emitter builds the complete UTF-8 harness line
with a literal LF, pass its exact byte length excluding the LLVM global's
trailing NUL, and branch on the returned status where success could otherwise
be reported. The Rust runtime writes and flushes that raw range without C
runtime text translation, NUL scanning, or delimiter insertion. A failure may
leave an already emitted prefix, so no generated path retries it. The boundary
flushes an existing Rust stdout buffer before the raw write and suppresses Unix
`SIGPIPE`, allowing a closed pipe to return the same failure status.

## Direct LCIR products

LCIR `Product` values become literal LLVM structs whose fields recursively use
their validated direct types. Tuple and record construction and functional
record mutation use `insertvalue`. A task-free tuple binding uses borrowed
`ProductExtract` operations. A Task-bearing ordinary direct structural tuple
instead uses one consuming `ProductSplit`, which emits one `extractvalue` for
each result in order. The split introduces no runtime call or ABI. Partial Task
projection and nominal, transparent, invariant-protected, or resource splits
remain unavailable. Parameters and ordinary results pass these structs by
value, and block parameters become aggregate phi nodes. LCIR source functions
do not allocate a universal value, tuple node, private record box, or GC object
for this representation.

Borrow-only contract inspection uses `TaskCarrierBorrow` for a complete
Task-bearing carrier, `ProductBorrow` for one Task-bearing field, and
`UnrefineBorrow` for a transparent Task-bearing wrapper. LLVM lowers these to
the same identity SSA or `extractvalue` operation as their task-free
counterparts; the distinction is checked ownership meaning, not an ABI or
runtime operation.

Projected access to a Task-free leaf inside a Task-bearing product uses one
atomic path operation. `TaskCarrierProject` lowers to nested `extractvalue`
operations while borrowing the complete owner. `TaskCarrierUpdate` consumes
that owner and lowers to nested `insertvalue` operations that produce the
updated aggregate. No Task-bearing intermediate owner, stack slot, or runtime
helper is introduced.

A synchronous mutable receiver is represented as one exact functional inout
value, independent of whether dispatch was inherent or selected through a
concept witness. An infallible call with source result `T` and ordered
writebacks `W...` returns `{ T, W... }`. A fallible call returns
`{ i32 status, T, W... }` and receives the usual fault-context pointer. Both
normal and fault exits carry the current receiver value, so a mutation completed
before a later fault remains visible to the caller. Direct `Bool`, `Int`, and
`Float` receivers remain ordinary `i1`, `i64`, and `double` SSA values. An
admitted task-free closed-sum receiver, including `Option` or `Result`, remains
its exact direct sum SSA value. An admitted projected primitive, record, or sum
receiver is extracted from and inserted back through its statically typed
aggregate path on the normal edge and before fault propagation on the unwind
edge. LLVM uses the existing scalar or aggregate SSA representation and
functional return ABI; no receiver pointer, proxy allocation, universal value,
or runtime writeback helper is introduced.
The owning synchronous `mut self` body may
reconstruct through its own top-level invariant product before its exit check;
the frontend and checked MIR reject every external or nested invariant
crossing. Unsupported projection shapes reject native preparation. Fully
instantiated task-free refined and invariant-record runtime construction
instead returns the exact typed
`Result[..., ConstraintError]`; open or unsupported-shape construction is a
coverage error.

Fresh-source proven record invariants and refined predicates do not add an LLVM
wrapper or check. LCIR retains their distinct semantic types and proof opcodes,
while the emitter forwards the already established physical SSA value. A
refined scalar therefore uses the base scalar ABI; a refined product uses the
base product ABI; and an invariant record uses its field product ABI. Supported
fully instantiated task-free refined and invariant-record runtime construction
returns the exact language `Result` value on typed LCIR; open or
unsupported-shape runtime construction rejects native preparation.
Serialized task-free refined and concrete task-free invariant-record proof
rechecks retain their nominal result shape on typed LCIR, guard publication with
the canonical `ArtifactProofRejected` runtime fault, and preserve concrete
generic contract types without a universal value.

Receiver-invariant restoration uses the same trust split without adding an ABI
operation. A fresh `RestoreReceiverInvariant(Proven)` emits no code. Its
artifact-normalized `Recheck` form derives the exact instantiated invariant from
parameter zero's nominal type, evaluates it against only the current receiver,
and raises `ArtifactProofRejected` through the existing cleanup/fault route.
It never requires old-parameter snapshots or trusts serialized contract text.

The current debug-info boundary describes that physical ABI as well. A
transparent scalar is reported as its base scalar debug type, and transparent
or invariant products use compiler-private physical product types; LLVM debug
metadata does not yet synthesize nominal source aliases such as `Money` or
`Range`. This deliberate display limitation does not erase nominal identity
from LCIR dumps, validation, cache fingerprints, or object artifact identity.

## Direct LCIR text

An admitted `Text` SSA value is one opaque LLVM pointer to a canonical object
whose prefix is the runtime layout descriptor pointer, allocation size, UTF-8
byte length, and Unicode scalar length, followed by exact UTF-8 bytes. A
literal-only artifact uses `ImmortalText`; each literal is a private,
unnamed-address global that points at `loom_layout_text_v1` and lives for the
process lifetime. If any reachable function concatenates Text or places Text in
a tuple, record, closed sum, or transparent/refined carrier, the entire
artifact instead uses `ManagedPointer` for every Text. Literals remain static,
while concat results are moving typed-GC leaves
with the same language-visible object shape. Products remain unboxed exact LLVM
structs; `ManagedPointer` is their Text-leaf provenance mode, not an aggregate
pointer. Neither representation is a universal `loom.Value`, a tagged interface
value, or a source-observable address.

LCIR loads scalar length directly from the immutable header. Containment calls
the existing allocation-free `loom_runtime_text_contains` byte-slice helper.
Equality and inequality compare content by combining byte length with that
helper; LLVM never compares literal object pointers to implement source
equality. The helper and descriptor declarations retain their exact target
pointer ABI when emitting 64-bit ELF, Mach-O, or COFF objects.

For managed Text, `TextConcat` calls
`loom_runtime_text_concat_typed_v1(left, right, out_cell)`. The helper validates
and copies both complete UTF-8 inputs into non-GC staging storage before its
first possible collection. It then allocates a typed leaf with a 32-byte fixed
prefix, 8-byte alignment, no pointer fields, and pointer-free trailing bytes.
Initialization contains no safepoint and the helper publishes the result only
after the header and bytes are complete. Resource exhaustion aborts as the
language's uncatchable OOM fault; every other nonzero status reaches a
fail-closed trap rather than a source unwind edge.

`TextGet` calls `loom_runtime_text_get_typed_v1(text, scalar_index, out_cell)`.
The helper stages the selected Unicode scalar before its possible allocation,
so collection cannot stale its source pointer. Status zero constructs `None`
without allocation, status one constructs `Some(Text)`, and every other status
traps as a compiler/runtime ABI defect. The result is the ordinary checked
unboxed sum carrier; only its active managed leaf is published to the typed
shadow stack.

The emitter derives exact backwards liveness for managed SSA values. A direct
Text value contributes one pointer-sized cell; a live product/sum contributes
stable candidate cells for deterministic managed-leaf projections, guarded by
active sum tags. Definitions and block parameters extract and publish every
such leaf. Per-site typed-root
bitmap state is published immediately before a collecting call, and aggregate
uses are reconstructed from post-safepoint leaf reloads. Results are excluded
at their own safepoint. Successor arguments are rooted only when the paired
explicit block parameter is live. Functions with no live-across managed leaf
emit no frame, descriptor, bitmap, push, or pop. Every normal, fault, and
resumed-fault return pops a frame that was pushed. Root-map ABI-limit overflow
is an emission-time `ProgramTooLarge` error.

The harness creates only a synchronous runtime when the root's exact effects
require one. Managed concat/get introduces no universal root chain, executor,
scheduler, suspension, or catchable fault channel. Transparent/refined carriers
reuse their base LLVM type and managed-root projections without a runtime box.
Unsupported dynamic Text producers reject native preparation.

### Direct managed Bytes

Canonical `Bytes` is one opaque managed pointer. `Text.encode_utf8` forwards
the exact immutable Text object pointer and performs no allocation. A Bytes
value may therefore carry the Text descriptor, while bytes materialized by
append or mutable push carry the distinct `loom_layout_bytes_v1` ByteObject
descriptor. The shared 32-byte prefix provides allocation and byte lengths;
for a ByteObject their difference is compiler-private spare capacity. The
descriptor and the ByteObject's required-zero reserved field prevent arbitrary
byte storage from masquerading as Text. Descriptor identity and capacity remain
compiler/runtime metadata and are not source RTTI.

Length and checked indexing load the header and trailing byte range directly.
Equality and inequality compare byte lengths and contents, never pointer
identity. These operations do not publish a root row or reach a moving-GC
safepoint. Negative and out-of-range indexes construct `None`; a found byte is
zero-extended to the source `Int` carried by `Some`.

Append calls
`loom_runtime_bytes_append_typed_v1(left, right, out_bytes)`. The runtime
validates both admitted descriptors, copies both complete immutable payloads to
non-GC staging storage, then allocates and publishes one initialized ByteObject.
Mutable `Bytes.add` first emits signed range checks and a RuntimeFault Assert,
then calls either `loom_runtime_bytes_push_typed_v1` or the independently
validated unique form `loom_runtime_bytes_push_unique_typed_v1`. The ordinary
form copies. The unique form may grow or reuse only a distinct ByteObject; a
Text-backed Bytes value always allocates, so Text remains immutable. Geometric
growth is capped by the collector's maximum object size and has no source-level
capacity contract.
`CollectionShare` is representation-identical SSA bookkeeping for a source
List or Bytes copy. LLVM reuses its input value directly and emits no machine
instruction or runtime boundary.
Decode calls
`loom_runtime_bytes_decode_utf8_typed_v1(bytes, out_text)`. A valid Text-backed
value returns the same pointer without allocation; a standalone ByteObject is
validated and may be relabelled in place as Text because both descriptors are
pointer-free and every existing Bytes alias admits either descriptor. Invalid UTF-8 selects
the ordinary nested `DecodeTextError.InvalidUtf8` variant. Positive or unknown
ABI statuses trap as compiler/runtime defects. LCIR treats append and both
push instructions as collection safepoints: the emitter publishes
the exact live root state before the call and passes a stable output cell.
Decode is non-collecting. Each runtime helper
publishes its fully initialized pointer as its final operation, so relocation
cannot stale an input or expose a partial result. Append may reuse the result's
direct root cell when one exists and otherwise uses a temporary. Push and decode
use stable output storage; after decode, LLVM constructs and publishes the
exact Result without an intervening safepoint. No universal value, executor,
JSON policy, or ownership/borrow syntax is involved.

## Direct typed Path

Canonical Path is an invariant-protected, unboxed one-field LLVM product whose
field is the exact direct Text pointer. The protection is enforced in checked
MIR/LCIR and adds no LLVM field. `PathFromText` scans the immutable Text byte range for NUL
and constructs `Result[Path, PathError]` in SSA. `PathAsText` is one
`extractvalue`. Construction reuses the existing non-collecting
`loom_runtime_text_contains` byte-range helper; extraction emits no call. These
operations do not allocate, publish a root state, or acquire an executor.

`PathJoin` extracts both Text pointers and calls
`loom_runtime_path_join_typed_v1(base_text, child_text, out_text)`. The helper
validates and copies both complete payloads into non-GC staging storage before
the allocation which may move either source, and publishes a fully initialized
canonical Text as its final operation. LLVM treats status `0` as success and
wraps that Text in the exact Path product; status `-1` constructs
`PathError.AbsoluteJoin`; every positive or unknown status traps as an ABI
defect. The stable output cell and exact backwards liveness use the established
typed shadow-root wire. Inputs not live after the call need no root because the
helper stages them before its safepoint.

The helper implements only Loom's portable lexical separator rule. It neither
queries a filesystem nor normalizes `.`, `..`, repeated `/`, host drive syntax,
or reverse solidus. It creates no runtime Path object, universal value, JSON
policy, executor, or ownership/borrow surface. Typed LLVM objects declare and
reference no untyped Path helper, and the runtime archive exports none.

## Direct managed Lists

A concrete closed `List[T]` is a direct managed pointer. Null is the canonical
empty value; nonempty objects contain `{ length, capacity }` followed by
target-data-sized, aligned element storage. LLVM recursively derives the
sorted, deduplicated union of exact managed-pointer byte offsets for each
element, including products, sums, and nested Lists, and supplies that
descriptor to `loom_gc_typed_repeated_alloc_v1`. Inactive sum pointer bytes and
unused capacity remain zero.

Ordinary append preserves value semantics by allocating and copying. A
validated `ListAppendUnique` may write the next element and then length in
place when the nonnull backing has capacity; growth remains a collecting
allocate/reload/copy path with geometric capacity. The root row includes the
old List and managed element even when dead afterward, and reloads both after
relocation. Length and get do not allocate, and get constructs the canonical
`Option[T]` sum directly.

List equality is already explicit LCIR control flow when it reaches LLVM. It
compares lengths, then uses nonallocating `ListGet` operations in a proved Int
loop and structurally compares the resulting `Option[T]` values. The emitter
does not call a generic equality runtime, expose an element pointer, or infer
aliasing from the managed backing. An allocation-pressure fixture crosses the
moving-heap threshold before these reads and verifies that exact typed roots,
not stable addresses, preserve the inputs.

## Direct typed TextMaps

A concrete closed `TextMap[V]` is also one direct managed pointer; null is its
canonical empty value. A nonempty object contains `{ length, entries[] }`.
Each target-laid-out entry is `{ Text key, V value }`, including ABI padding,
and entries stay sorted by UTF-8 key bytes so lookup and replacement are
deterministic without a hash seed or runtime type metadata.

LLVM derives one exact repeated descriptor for each concrete map type. Offset
zero of every entry traces the managed Text key, and the remaining offsets are
the sorted, deduplicated managed leaves of the exact `V` representation. This
covers scalar, Text, product, closed-sum, List, and nested TextMap values. The
descriptor uses the existing `loom_gc_typed_repeated_alloc_v1` ABI; construct,
insert, length, containment, lookup, removal, and structural comparison are
typed LCIR operations emitted directly, not type-erased runtime map calls.

Insert locates the canonical position before its safepoint, allocates exactly
the new logical length, then reloads the rooted old map, key, and managed value
leaves after a possible relocation. It copies the prefix and suffix and writes
the replacement or inserted entry. It never mutates the source backing, so a
shared alias retains its prior logical value. A future reuse path may be added
only behind an independently validated uniqueness certificate. Length and
`get -> Option[V]` and `entry_at -> Option[(Text, V)]` do not allocate;
lookup and canonical-order indexed access construct their exact option carriers
without a universal `Value`, witness/executor pointer, tag registry, or stable
address assumption.

Containment reuses the same sorted-key locator and returns its exact found bit.
Removal also locates before its possible safepoint. A missing key returns the
source pointer, removing the sole entry returns null, and every other successful
removal allocates `length - 1` entries, reloads the exactly rooted source map,
then copies the prefix and suffix around the removed entry. The key is not used
after allocation and therefore needs no forced root. Structural equality
compares lengths and reads sorted entries positionally through the same checked
exact `Option[(Text, V)]` operation exposed by source `entry_at`. This keeps
insertion order unobservable and recursively uses the ordinary typed equality
CFG for `V`.

## Collision-free closed-sum storage

Every payload-bearing tagged sum receives one deterministic target-layout
plan. LLVM target data supplies each variant payload's ABI size/alignment and
the recursive managed-offset walk supplies its pointer cells. Pointer-width
ranges are classified as pointer bytes; every other payload byte, including
padding, is non-pointer. The planner visits pointer-free variants first in
source order, followed by pointer-bearing variants in source order, and gives
each the lowest ABI-aligned offset where pointer and non-pointer classes do not
cross. Same-class bytes may overlap. Construction starts from a zero carrier,
then writes only the active payload; match control flow reads payload bytes only
after selecting their tag case.

One backend-owned cache and in-progress set cover sum layouts and recursive
managed-offset walks for the complete emission. A shared 65,536-step graph
budget prevents nested sums from restarting work or expanding exponentially.
Carrier bytes are limited to 64 KiB, and all carrier plans in one artifact
consume one 65,536-byte-step placement budget. Every pack and unpack charges
its target ABI payload size to an independent shared 65,536-byte emission
budget before generating bytewise LLVM instructions. Checked arithmetic covers
every extent and alignment calculation. Cycles, invalid pointer cells, and
exhausted planning or emission bounds fail closed before LLVM can emit an
unsafe or unbounded artifact. Checked wide-sum source regressions prove that
neither budget resets between layouts or construct sites. The independent LCIR
validator continues to validate semantic sum identities; it does not reproduce
target-specific physical bytes.

This general rule keeps the canonical recursive `Json` at 24 bytes on
supported 64-bit targets, with Bool/Float bytes at physical offset 8 and its
managed cell at offset 16. `List[Json]` derives stride 24/pointer offset 16 and
its TextMap entry derives stride 32/pointer offsets 0 and 24. A separate
`Choice(Number(Int), Label(Text), Pair)` receives the same compact safe shape.
Two 16-byte record variants with pointer/scalar cells in opposing order receive
different carrier offsets, producing a 32-byte sum whose exact pointer cells
are bytes 8 and 24.
An `Outer(Json, (Int, Int, Int))` value is 40 bytes with the nested managed
cell at offset 32. Pack/unpack, active managed-root rebuild, and List/TextMap
descriptor construction all consume this exact plan. There is no
Json-specific condition, universal envelope, runtime tag registry, tracing
callback, or executor. Unsupported 32-bit managed layouts fail closed.

Canonical Json construction, copying, List/TextMap operations, exhaustive
matching, exact moving-GC roots, structural equality, parsing, and formatting
lower through the same typed artifact. Both public JSON functions are ordinary
Loom source. Formatting reaches ordinary direct calls, matches, collection
operations, Float primitives, unique packed byte pushes, and `BytesDecodeUtf8`;
LLVM has no Json-specific instruction, layout descriptor, status mapping, or
runtime declaration.

## Direct typed File and Socket I/O

LCIR `IoTaskCreate` carries one of seven closed operations and an explicit
`Result` or `Fault` error mode. LLVM derives an immutable typed-task descriptor
from the exact direct output. Result mode stores
`Result[T, IoError]`; fault mode stores `T`. Both frames include the one managed
scratch Text root required while the runtime publishes an error message.

Creation calls `loom_typed_io_task_create_v1` with a copied primitive request.
The generated callback advances the leaf through `loom_typed_io_poll_v1` and
accepts only the operation's resource, Text, Unit, or closed error outcome. On
success it writes the exact target-native value into the frame. On an ordinary
host error, Result mode constructs the canonical direct `Err(IoError)`, while
fault mode records the operation-specific Task fault and returns the faulted
step. Cancellation uses `loom_typed_io_cancel_v1`; lexical File/Socket disposal
calls the selected source witness, whose private leaf uses
`loom_typed_resource_close_v1`.

The runtime wire remains `typed-io-v1` plus `typed-resource-v1`. It transports
no source layout, universal value, or nominal type ID. The former
`loom_file_*`, `loom_socket_*`, and `loom_io_close` symbols are absent from the
runtime and emitter.

## Direct typed Tasks and fixed joins

On the currently pinned 64-bit typed-task ABI, each `AwaitTasks` terminator
carries one or more ordered canonical Task handles. Its checked coroutine-plan
row records the join mode and exact output type for every child, followed by the
exact live values forwarded across suspension. LLVM stores all child handles and
live values in the target-laid-out suspension row, prepares one structured
mode-specific join, and adds each child in source order. The terminator has
explicit normal, fault, and cancellation targets with one identical exact live
row. On normal `all` completion LLVM takes every result using that child's exact
size and alignment. On normal `any` completion it takes only the selected
winner. `settled` forwards every terminal child handle, while `race` forwards
only the terminal winner handle; explicit `TaskOutcomeTake` instructions then
construct the canonical outcomes. Every mode reloads the live row and enters
the normal target with the mode-derived values first. A child fault activates the source fault before
entering its target; a cancel path forwards only the same live row and preserves
inactive source-fault state. A normal one-child await is the `all` path with an
arity of one.

The generated source-coroutine resume callback is also the descriptor's cancel
callback. Its prologue checks the Task cancel-request bit before dispatching by
frame state. A coroutine with `requires` also has three `i64` frame fields for
the creating call's file, start, and end coordinates. Its constructor stores the
`TaskCreate` origin there; the root harness stores the root declaration span.
State zero checks those preconditions before the body and emits the same
contract JSON schema with the carried blame. The ordinary dispatch enters state
zero or the structured join step. The cancellation dispatch terminates state
zero directly and, for a suspended state, reloads the row and enters that
await's checked cancel target. This uses the existing typed-task cancellation
query and callback ABI. Stored fixed Task-policy composites continue to use the
shared generic cancel callback.

An immediately awaited fixed tuple or fixed Task-policy call lowers directly to
multi-child `AwaitTasks`, then constructs the exact result in the continuation.
A stored fixed policy uses the `TaskJoin` instruction. `all` produces the exact
value tuple, `settled` its exact outcome tuple, `any` one homogeneous winner,
and `race` one homogeneous winner outcome; one-child tuple modes still produce
a one-field tuple. The emitter generates a target-laid-out composite frame and
an immutable typed-task descriptor for each distinct mode, child-output row,
and result type. Matching `all`, `settled`, and `race` sites reuse that shape;
`any` also includes its producer origin so the shared callback can record exact
`TaskAnyFailed` blame. The callback publishes only the statically known managed
leaves.

The composite is initialized while unpublished. Generated code then passes a
temporary contiguous child-pointer array to
`loom_typed_task_publish_adopting_v1`. That call validates and reserves the
complete ownership transfer before atomically replacing the active parent's
selected child edges with the published composite. A nonzero status leaves the
topology unchanged; generated code aborts the unpublished frame and traps rather
than entering a partially adopted graph. Neither direct nor stored fixed joins
call the universal join constructor or universal join-result helpers.

A nonempty, immediately awaited, fixed-arity `Task.any` is direct when all child
outputs have one exact type. The scheduler retains the original winner ordinal,
cancels unfinished losers, and drains every child. Consuming the valid join
completion through `loom_task_join_step` then disposes completed loser results
and retires all losers exactly once in reverse-input order. Generated code
switches on that original ordinal and loads the winner pointer from the
coroutine frame's corresponding static child field, so shrinking the runtime
join list cannot change the selection. A loser-disposal fault changes the join
step to faulted before coroutine cleanup; with no winner, LLVM raises
`TaskAnyFailed` at the source `Task.any` expression before entering that
cleanup. Immediate fusion writes that producer origin onto `AwaitTasks`.
Source-coroutine cancellation dispatch bypasses this ordinary resume operation
and cannot manufacture `TaskAnyFailed`.

Fixed `Task.settled` and `Task.race` use the same direct suspension rows.
Generated `TaskOutcomeTake` code calls
`loom_typed_task_take_outcome_v1(task, value_out, size, align, code_out,
message_out)`, switches over its terminal status, and constructs the exact
`Completed(T)`, `Faulted(TaskFault)`, or `Cancelled` sum. Fault Text allocation
is a visible safepoint; the LCIR root plan therefore protects outcomes already
constructed across subsequent captures. `race` shares generalized winner
finalization with `any`, retaining the original winner while disposing and
retiring losers in reverse source order.

A successful completed-result take moves the child's resource-ledger entries,
which back any published File or Socket capability tokens, to its active owner
Task before retiring the child. A direct root take leaves those entries in the
root Task ledger until explicit close or executor teardown. Faulted and
cancelled outcomes transfer no entries, and completed losing or unconsumed
children instead close every concrete resource left in their ledgers before
retired-task memory reclamation. This ordering prevents reclaiming a resource
whose capability has already been delivered to its consumer, without adding an
LCIR ownership field or a new typed-task ABI argument.

The runtime does not trust generated call order for this ownership commit. A
child take requires exact membership in both owner rows, a settled successful
join, the matching ALL/ANY result or SETTLED/RACE outcome shape, and completed
ANY/RACE winner finalization. A hostile early or wrong-shaped call therefore
cannot remove a child before loser disposal or reinterpret a terminal state.

`TaskJoinList` uses a separate exact dynamic composite shape. Its frame stores
state, the source List carrier, and the mode-specific result. Descriptor root
states keep the source live while child handles are read, keep source and
partial result live during collecting captures, and retain only the completed
result after publication. Task elements are stable scheduler pointers, so the
source List descriptor has no managed element offsets.

For a nonempty List, construction passes its data pointer and runtime length
directly to `loom_typed_task_publish_adopting_v1`; no stack copy or universal
join object is created. Empty `all` and `settled` publish a normal completed
composite with an empty List result. Empty `any` and `race` record canonical
`EmptyTaskJoin` at the producer origin. The callback uses the ordinary
prepare/add/suspend/step/winner/take protocol in a runtime-counted loop and
reloads rooted source and result carriers after every collecting call.

The fixed-row slice remains static and nonempty: a sole nonempty List literal
is flattened into it without an input List allocation, and `all` or `settled`
build their List result after resume. Stored, computed, empty, and
runtime-sized homogeneous Lists use `TaskJoinList` instead of selecting the
fixed-row operation. The frontend currently maps canonical, unshadowed Task API
members through its temporary catalog to a compiler-private `TaskIntrinsic`
before MIR construction; LLVM never inspects their source spelling. This enum
is a transitional frontend bridge, not a standard-library identity or ABI.

The completed source boundary resolves every public Task member to an ordinary
source `DefId` in `std`, and ordinary reachability retains its body and only the
private primitives that body calls. LLVM continues to implement typed
join/select readiness, exact result-or-outcome extraction, and structured
cancellation-and-drain, but it never maps a public source definition back to a
policy enum. Moving the policies to source therefore deletes both the temporary
catalog and `TaskIntrinsic`; public policy names do not become language
operators.

## Direct lexical cleanup

LCIR contains the already expanded control flow for `defer`, `scoped`, and
source assertions. The LLVM emitter never reconstructs lexical scope and never
allocates a runtime cleanup stack. Each normal block exit, return, or fault edge
enters its statically emitted newest-first suffix. If cleanup starts with a
fault active, a later cleanup fault is suppressed while older cleanups continue
and the original fault remains primary.

An await with active cleanup has two additional compiler-expanded suffixes.
Child fault enters the static LIFO suffix with source fault active and finishes
at `ResumeFault`. Cancellation enters the corresponding static LIFO suffix with
source fault inactive and finishes at `TaskCancelled`. Cancellation cannot
suspend. If one of its cleanup actions faults, the established runtime
cancellation remains primary, suppresses the cleanup fault, and continues older
actions. The emitter therefore needs neither dynamic cleanup registration nor a
second resource representation for suspended scopes.

Scoped disposal is an ordinary monomorphic source-witness call or fallible
invoke with functional receiver writeback. Canonical File and Socket witness
bodies call their authenticated private close leaves, which emit
`loom_typed_resource_close_v1(executor, kind, token_cell)`. The token cell is
allocated once in the LLVM entry block for each leaf call, so a cleanup edge
executed by a loop cannot grow the stack. The helper resolves the capability
against the active Task's unique runtime owner, closes it, writes the
invalid-token sentinel, and does not schedule, enqueue, suspend, or drive an
executor. An invalid or already-closed sentinel is rejected. The instruction
rebuilds the exact closed resource value before the next cleanup action. There
is no universal `loom.Value`, indirect witness call, source close fault, or
synchronous executor route.

Emission follows the validator's exact cataloged canonical `File`/`Socket` kind
agreement. The returned status is switched explicitly: `0` produces `Unit` and
the closed resource, while every other value calls `llvm.trap` and is
unreachable. An invalid, stale, sibling-owned, or opposite-kind live token is an
ABI defect, never a source-level close failure.

Managed return values captured before a deferred collecting call remain normal
LCIR SSA liveness. The root planner expands their Text-bearing product leaves,
and the emitter rebuilds the product after relocation before returning it; no
cleanup-specific GC representation is needed.

## Direct LCIR closed sums

LLVM derives every sum layout from the checked `SumRepr` and target data. A
single variant is its payload struct with no tag. A multi-variant enum whose
variants have no payload fields is only the smallest checked integer tag. All
other sums are `{ tag, carrier }`. The carrier has the maximum payload ABI
size, the maximum payload ABI alignment, and the required tail padding. A
zero-length array of the most-aligned payload type imposes alignment without
adding storage; target-data checks reject any disagreement between the planned
and actual carrier size or alignment.

`SumConstruct` builds payload fields in source order. `SumSwitch` extracts the
tag once, switches exhaustively, and decodes the selected carrier into typed
payload block parameters. Temporary typed carrier storage is an LLVM lowering
detail: the release optimization gate requires SROA to remove every such
`alloca` and forbids `memcpy`, the universal `loom.Value`, execution-runtime,
GC, and executor symbols for pure sums, and indirect calls. The output-only
`loom_runtime_stdout_write_v1` harness declaration is allowed and does not
weaken the pure source-function check.

`SumBorrowSwitch` emits the same tag dispatch and payload extraction, but LCIR
marks Task-bearing payload parameters as borrowed aliases and keeps the
scrutinee owner live. The validator prevents those aliases from reaching a
consuming boundary, so LLVM needs neither a clone nor a runtime ownership call.

Structural sum equality uses one checked `SumZipSwitch`. LLVM extracts both
tags once, branches unequal tags directly to false, and switches once on the
shared matching tag. Only the selected case decodes the left and right carriers
into their exact typed payload fields, in that order. Tag-only enums need no
carrier decode, and a single-variant tagless sum enters its sole case directly.
The CFG and emitted dispatch are linear in the variant count. No comparison
reads the carrier as raw bytes, so padding and inactive managed-pointer
candidates do not participate in language equality.

The test harness consumes the checked artifact's `TestOutcomePlan`. `Unit`
tests pass after a successful call. `Result[Unit, E]` tests compare the physical
tag with the explicit success variant; the explicit failure variant produces a
normal failed-test status. A source `RuntimeFault` is checked independently
before the result tag and retains the existing runtime-failure behavior.

Harness stdout is success-sensitive. Failure to write or flush `Unit` or a
passed-test line changes the otherwise successful process status to nonzero.
Failure while writing an already failing diagnostic leaves the existing
nonzero status intact; because a prefix may already be visible, the harness
does not retry or add a second diagnostic.

## Object identity and linking

The authoritative current LCIR dump, checked-artifact, native-object,
object-cache, checked-MIR, and runtime ABI identities are maintained in
[Versioning and compatibility](../project/versioning.md). The native-object
identity streams the canonical complete checked-artifact identity and includes the compiler/backend build fingerprint, linked LLVM
version, native runtime ABI, exact normalized triple and data layout, CPU and
feature policy, implicit-versus-explicit target selection, optimization
pipeline, PIC relocation, and stable debug-source metadata. Output and LLVM-IR
side-artifact paths are excluded. Requesting an IR side artifact bypasses the
object cache so the file is always produced; cache lookup never suppresses a
fingerprint error.

The LCIR identity covers the explicit transitive effect lattice, canonical
typed fault and proof-replay metadata, source-contract placement, every direct
representation and operation, collision-free closed-sum layout, exact managed
root plans, finite dynamic catalogs, lexical cleanup, and the complete checked
coroutine plan. It also records closed static-witness selection and normalized
associated types, although those proof facts do not become machine-ABI
parameters. For a `MAY_FAULT` coroutine, each resume callback creates an
activation-local fault context attached to the current executor; normal return
publishes the exact typed value, while fault and cancellation use distinct
scheduler step codes. Callback-local typed roots are popped on every terminal
or pending exit.

Generated typed LLVM objects use narrow typed runtime helpers rather than a
universal value boundary. The runtime archive exports no universal GC/value,
witness, legacy Task, value-operation, or Int-list surface. Typed repeated
allocation and shadow-root descriptors carry exact managed layouts. Text,
Bytes, Path, and Float-formatting helpers validate their direct inputs, stage
every borrowed byte sequence before a possible collection, and publish only
fully initialized managed results. Structured logging receives
the canonical `LogLevel`, direct Text, and an optional contiguous
`TextMap[Text]` entry view; it is non-collecting, maps status `2` to
`LogWriteFault`, and traps on an invalid status. Reachable logging lowers
through typed LCIR; the runtime exports no universal-value logging boundary.
The stdout helper remains an output-only
boundary and does not create a runtime or executor requirement.

Typed Task helpers create timer leaves, atomically adopt static join children,
finalize `any` and `race` losers, and take exact terminal outcomes. Their
preflights validate complete ownership and join topology before mutation.
State-zero preconditions carry optional creation-site blame in the generated
frame and reuse the established fault-context wire.

File and Socket values carry monotonic runtime capability tokens, never raw OS
descriptors or handles. `IoTaskCreate` uses exact task frames, immutable
descriptors, and one compiler-generated completion callback per operation,
error mode, and direct output layout for the seven closed operations. The
runtime copies borrowed Text,
resolves a source token only through the active Task's unique ledger entry, and
duplicates the concrete resource before retaining an operation. Open, create,
and connect completion insert the concrete RAII owner into the child Task
ledger before generated code can observe its token. Exact result or outcome
take transfers that ledger to the active owner before child retirement; close
removes only a kind-matching token from that same active Task ledger. A
cancelled Task can do so only inside the executor's guarded, non-suspending
cleanup activation; resource reads, writes, and new work remain rejected there.
The bundle exports only the versioned typed I/O and typed resource boundaries;
there are no universal File/Socket wrappers, universal close function, or
fixed runtime File/Socket nominal IDs.

The focused I/O ABI test covers dead-I/O reachability, typed symbol presence,
and obsolete-symbol absence. The dedicated source fixture runs real CLI
`check/build/test/run`. The integrated standard-library native test uses the
production preparation facade and drives
real filesystem and loopback Socket operations on its host test platform.

Integer parsing is ordinary `std.int` source. The native emitter retains no
integer-parser instruction, special emission path, runtime symbol, tombstone,
or compatibility decoder.

Every executable link consumes one validated runtime bundle; the compiler
contains no runtime archive and its build script never starts Cargo. The CLI
discovers a host bundle from an explicit option, the environment, or the
installed sibling directory. Cross-target linking additionally requires an
explicit linker. Object emission is independent of this link input. Final
native executables are not persistently cached because the system linker, SDK,
and debug-companion environment are not yet hermetic.

Linking copies the validated runtime archive to one adjacent private snapshot
per invocation and synchronizes it. Before starting an external linker, the
compiler closes both the writable construction handle and the writable clone
temporarily retained for its first identity check. An independent read-only
identity anchor survives that handoff. The final Windows handle permits
concurrent readers but denies writers and deletion, matching MSVC input-library
sharing without globally serializing links or reopening a snapshot to
replacement. The compiler rechecks both file identity and SHA-256 after linking.

## Debug information

The typed LCIR backend emits source line information from stable
project-relative paths. Linux executables retain DWARF in the ELF output. On
macOS, `dsymutil --verify` produces a sibling `.dSYM` bundle. `loom debug`
keeps temporary executable and debug data alive for the debugger session and
launches in the project root. LCIR publishes compile-unit, file,
`DISubprogram`, physical callable-signature, formal-parameter, parameter-value,
and instruction-location metadata. LCIR does not retain source parameter names,
so visible parameters have stable debugger names `arg0`, `arg1`, and so on.
Debug-source file IDs must be unique and must cover every emitted `Origin`;
missing or duplicate identities are compiler errors rather than mappings to the
primary file at an invented `(1, 1)` location. Hand-built LCIR using a synthetic
origin must therefore provide that generated file explicitly when requesting
debug information.

The signature deliberately describes the exact compiler ABI rather than a
logical wrapper that does not exist. Direct products use stable compiler-private
`LoomProduct<tN>` names because LCIR does not retain source record names; their
members, size, alignment, and offsets come from LLVM target data. Closed sums
similarly use `LoomSum<tN>` and describe their exact tagless, tag-only, or
tagged physical ABI. Tagged carrier and tag fields are artificial debug members
with target-data-derived sizes, alignments, and offsets. An infallible
inout callable returns `{ value, writebacks... }`, while a fallible callable
returns `{ status, value, writebacks... }` and receives an artificial trailing
`LoomFaultContext*` parameter. Status and writeback members are artificial.
These names describe compiler implementation types, not Loom source types or a
stable native ABI. In particular, a debugger's step-out result is the complete
physical aggregate; it must not interpret the status field as the logical
result. `loom debug` uses the same typed preparation as build, run, and test.
Development optimization alone is not a debugger contract.

MSVC-targeted objects carry the LLVM `CodeView` module flag, and the linker is
given `/DEBUG` and an explicit staged `/PDB:` output. The Windows release entry
checks typed-LCIR COFF and PDB production, but source-level debugger
behavior remains partial and is not claimed until a native debugger test
exists.

There is no stable native library, debugger pretty-printer, plugin, or FFI ABI
in the current implementation.
