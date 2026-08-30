# Value layout and native ABI

The native layout described here is compiler-private. It exists so generated
LLVM and the Rust runtime agree within one toolchain build. Source code cannot
inspect tags, pointers, allocation addresses, witness descriptors, or calling
conventions, and external code must not depend on them.

Production native compilation selects one representation boundary for an
entire reachable artifact. A completely supported direct artifact uses typed
LCIR for primitive values, direct `Text`, one-pointer typed `Bytes`, structural
tuples, one-field typed `Path`, closed records, compile-time-established refined
values, and eligible closed enums. Any reachable feature outside current LCIR
coverage selects the complete checked-MIR layout below; the two callable ABIs are
never mixed in one object. In particular, supported nongeneric `.loomi` MIR
`Recheck`
constructions replay their predicate in typed LCIR before entering the
transparent representation. Generic or otherwise unsupported proof replay
uses the checked-MIR checker.

## Universal value envelope

The complete fallback representation is `ValueSlot`: six 64-bit words.
Current words carry a value tag, nominal metadata, auxiliary data, scalar data,
a managed-data pointer, and a witness pointer. Different value kinds use
different subsets and zero unused words.

The tag lets shared runtime helpers clone, compare, trace, format, and destroy
values whose static type has been erased by the current generic/universal
lowering. It is not language reflection or a permanent per-value type ID.
File and Socket operations are no longer admitted at this boundary: reachable
I/O must lower completely through typed LCIR and cannot construct a universal
Task result or resource record.

The current runtime ABI identity is versioned as a whole in
`loom-runtime-abi`. That identity is checked in runtime bundles and object
linking. It is not backward-compatible with earlier identities and is not a
public ABI.

## Native harness stdout

Compiler-generated native run and test harnesses call
`loom_runtime_stdout_write_v1(data, length) -> i32`. `data` names an exact raw
byte range and `length` is its `u64` byte length. A zero-length call may use a
null pointer; a nonzero call requires a readable range representable by the
runtime. The boundary does not scan for NUL, append a delimiter, or pass bytes
through C-runtime text-mode translation. Harnesses construct the complete UTF-8
line themselves, include one literal LF byte when a line ending is intended,
and exclude the LLVM string global's trailing NUL from `length`. Consequently,
redirected Windows output retains LF rather than being rewritten to CRLF.

Status `0` means that the complete range was accepted and stdout was flushed;
status `1` rejects an invalid argument, and status `2` reports a write or flush
failure. I/O failure may occur after a prefix was written, or after all bytes
were accepted but flushing failed, so generated callers must not retry. Failure
while emitting an otherwise successful `Unit` result or passed test makes the
process result nonzero. An already failing run or test keeps its original
nonzero result and does not recursively report the output failure.

The runtime holds the standard-output lock and flushes any existing buffered
Rust stdout prefix before writing through a cloned raw OS handle. On Unix, the
first boundary call installs process-wide `SIGPIPE` ignore semantics so a
closed pipe reports status `2` instead of terminating a compiler-generated C
entry by signal. A zero-length call still performs this flush and may therefore
fail even though its null data pointer is valid.

This is an output-only runtime boundary. It does not give a pure typed LCIR
source function a runtime context, GC capability, or executor requirement. A
pure executable object may nevertheless reference this symbol from its native
harness. The boundary is identified by `stdout-v1`; the complete current runtime
identity is defined in
[Versioning and compatibility](../project/versioning.md).

Typed logging uses a separate synchronous, non-retaining ABI. Its message is a
complete direct Text object pointer, and its optional fields are a contiguous
view of canonical `TextMap[Text]` entries, each exactly two pointers. Empty
fields use a null pointer and zero count. The runtime validates Text headers,
UTF-8, field ordering, and bounds before producing one compact JSONL line. The
call does not enter the Loom collector or scheduler. `typed-log-v1` identifies
this compiler-private boundary.

## Typed JSON formatting

Direct Json formatting uses the synchronous
`loom_runtime_json_format_typed_v1` boundary. The compiler publishes one
immutable `LoomTypedJsonLayout` whose target-derived offsets describe the
complete recursive Json carrier plus its Text,
`List[Json]`, and `TextMap[Json]` leaves. The call borrows the descriptor and
input carrier and receives its managed Text through a stable output cell; it
does not retain either input or consult a nominal type registry.

The runtime validates the descriptor, traverses all six canonical variants,
and stages the complete compact JSON byte sequence outside the moving heap.
Only success allocates and publishes Text. Depth exhaustion and non-finite
numbers return distinct statuses which LLVM maps into the exact direct
`JsonError` variants. `typed-json-v1` identifies this compiler-private
boundary.

## Typed Bytes

Canonical `Bytes` uses one direct managed pointer on supported 64-bit targets.
`Text.encode_utf8` preserves the exact immutable Text object pointer, so this
conversion allocates nothing and the resulting Bytes retains the Text layout
descriptor. Bytes materialized by append instead use a distinct ByteObject and
`loom_layout_bytes_v1` descriptor. Both forms have the same checked 32-byte
prefix and trailing-byte position, but only the Text descriptor carries a
validated Unicode-scalar count. Arbitrary byte storage therefore cannot
masquerade as Text.

Length and checked indexing load the immutable header and trailing byte range
directly. Content comparison examines byte lengths and contents rather than
pointer identity. None of these operations reaches a moving-GC safepoint.
Append calls `loom_runtime_bytes_append_typed_v1`; decode calls
`loom_runtime_bytes_decode_utf8_typed_v1`. Each runtime boundary validates its
admitted descriptor forms and stages every source byte before an allocation can
move the heap. Append publishes a fully initialized ByteObject last through a
stable output cell, which generated code may reuse as the result's direct root
cell when one exists. Decode publishes through a stable temporary, then
generated code constructs and publishes the exact Result without an intervening
safepoint. It returns the shared pointer for a valid Text-backed value, allocates
canonical Text for a valid standalone ByteObject, and reports invalid UTF-8 as
the ordinary `DecodeTextError.InvalidUtf8` result.

Generated objects reference the two operation symbols and the established
typed-root wire. The Bytes descriptor and typed allocator remain runtime
implementation details rather than code-generation dependencies. The current
boundary is identified by `typed-bytes-v1`. Bytes adds neither a JSON policy nor
ownership or borrow syntax.

`Text.from_utf8_units` borrows a direct `List[Int]` as a contiguous `i64`
range only for one synchronous call. The runtime narrows and stages every unit
before entering the moving allocator, validates the complete UTF-8 sequence,
and publishes one canonical Text pointer last. An empty List crosses as
`null + 0`; generated code never forms an interior pointer from the null empty
representation. Out-of-range units and malformed UTF-8 return the sole
negative language status. The format-neutral boundary is identified by
`typed-text-units-v1`. It does not expose List layout as a public ABI and does
not add a JSON runtime.

## Typed Path

Canonical `Path` remains an unboxed one-field product containing Text. Its
LCIR semantic kind is invariant-protected, so generic record construction and
field insertion cannot bypass `Path.from_text`; this protection adds no runtime
field or tag. It has no runtime-owned Path object, platform handle, or
filesystem identity.
`Path.from_text` scans the canonical Text bytes for U+0000 and constructs the
exact `Result[Path, PathError]` without allocating. `Path.as_text` extracts and
returns that same immutable Text pointer. Neither operation is a moving-GC
safepoint.

`Path.join` extracts the base and child Text fields and calls
`loom_runtime_path_join_typed_v1`. The runtime validates and stages both
complete UTF-8 payloads before its sole possible managed allocation, inserts
one `/` only when the portable lexical rule requires it, and publishes the
fully initialized Text last through stable output storage. Status `0` is
success, status `-1` is the ordinary `PathError.AbsoluteJoin` outcome, and any
positive or unknown status is a compiler/runtime ABI defect. Exact managed-root
liveness keeps only values live after the call in the typed shadow frame.

The boundary is identified by `typed-path-v1`. It adds no filesystem lookup,
host path normalization, JSON behavior, or ownership/borrow syntax. The
whole-artifact MIR route uses its own path helper symbols; a native object never
mixes the two routes.

## Typed external resources

Canonical `File` and `Socket` are protected empty source records that lower to
direct one-field products containing an `Int` runtime capability token. The
token is not an OS descriptor or handle. The concrete RAII owner remains in the
runtime Task ledger, and every operation resolves the token against the current
active, running owner before it clones or closes the resource. Generic product
construction, extraction, and insertion cannot forge or expose these
capabilities.

`loom_typed_resource_close_v1(executor, kind, token_cell)` borrows a stable
token cell for one synchronous call. Status `0` closes the resource and writes
the invalid-token sentinel. Every nonzero status is a compiler/runtime ABI
defect; final RAII release has no ordinary failure result. No task is scheduled
and no managed allocation can occur at this boundary.

The active Task must own the token exactly once with the requested File/Socket
kind. An untracked, stale, sibling-owned, opposite-kind, or duplicate token is
an invalid ABI call and cannot reach any unsafe host-handle operation. Tokens
are monotonically allocated for the process lifetime and never reused, so a
closed capability cannot accidentally name a later resource even when the OS
reuses its own descriptor value. File/Socket cleanup is final RAII release; a
future durability guarantee must come from an explicit flush/sync operation.

All seven File/Socket Task operations share the `typed-io-v1` request/outcome
wire. The wire reports only an operation, copied byte views, a resource token,
primitive outcome kind, closed error-kind index, closed fault class, and managed
Text scratch root; it carries no source `TypeId` or physical `Result` layout.
The fault class distinguishes invalid ports and name-resolution failures from
connection failures without exposing a universal error value. The LCIR
instruction records whether generated code constructs
`Task[Result[T, IoError]]` or the faulting `Task[T]` form. The runtime therefore
needs neither universal `loom_file_*`/`loom_socket_*` wrappers nor fixed
File/Socket nominal IDs.

The current exact runtime identity is defined in
[Versioning and compatibility](../project/versioning.md). Typed-task ABI v1,
typed-I/O v1, typed-resource v1, typed-process ABI v1, coroutine v2, wait v1,
standard-library ABI v7, Text v3, and GC v9 identify the corresponding current
components.

## Checked-MIR primitive and aggregate specialization

Within a complete checked-MIR object, `Unit`, `Bool`, `Int`, and `Float` can use
direct private LLVM values in eligible internal calls. Monomorphic records with
no invariant and only direct primitive fields can use a separate closed-world
specialization:

- ordinary by-value parameters and readonly receivers are aggregate values;
- a mutable receiver uses a call-scoped in/out pointer;
- results are returned directly when the closed-world fault requirements allow
  it, or with an internal status when they do not.

Unsupported specialization boundaries materialize the independent universal
representation. This preserves value-copy semantics and prevents a private
stack address from escaping. These layouts are checked-MIR emitter decisions and
are not reused by typed LCIR.

File and Socket I/O is an explicit exception to universal fallback. Direct
checked-MIR fingerprinting and emission reject every reachable operation and
close builtin, and production preparation fails closed if the complete I/O
artifact cannot remain on typed LCIR.

Records, enums, refined values, tuples, and generic lists on the universal path
use GC-managed nodes. Logical copy is independent: mutating one value cannot
write through an earlier copy merely because the runtime shares an allocation
internally.

## Typed LCIR representations

The independent `loom-codegen-ir` foundation catalogs `Unit` as `Zst`, `Bool`
as `I1`, `Int` as `I64`, `Float` as `F64`, and `Text` and canonical `Bytes` as
opaque pointers on 64-bit targets. Text uses `ImmortalText` for a literal-only,
product-free artifact and `ManagedPointer` for the entire artifact when concat
or a Text-bearing product is reachable. Bytes always uses `ManagedPointer` and
admits only the compiler-proven Text-backed and ByteObject descriptor forms.
Each supported structural tuple, canonical Path, or closed record is an
immutable `Product` of canonical element or field value types. Path has exactly
one canonical managed Text field and uses the invariant-protected product kind;
its LLVM layout is unchanged. Tuples and records may contain one another
and managed leaves as long as the representation graph is acyclic. The product
itself remains an unboxed aggregate. An explicit registration table
chooses the canonical value representation for a semantic type; other
representation alternatives do not compete merely because they have the same
semantic type.

An established monomorphic refined type receives its own semantic
`ValueTypeId` and reuses the exact `ReprId` of its declared base. The checked
plan records that relationship, so `RefineProven` and `Unrefine` cannot be used
as arbitrary same-layout casts. A record whose invariant was proved uses a
protected product type: `InvariantRecordProven` may create it, while ordinary
product construction or insertion may not bypass the invariant boundary.

The checked-artifact LLVM API maps a product to a literal LLVM struct and emits
construction, projection, and functional field replacement as `insertvalue`
and `extractvalue`. Product parameters, returns, block phis, and loop-carried
values remain direct SSA. Ordinary product copy or move copies the SSA value;
mutation reconstructs the changed path and therefore cannot write through an
earlier copy. No LCIR product requires a `ValueSlot`, record allocation, GC
trace metadata, executor, or source-function `alloca`.

An eligible closed enum is also a direct value. A single variant is its payload
without a tag; multiple empty variants use only the checked minimal integer
tag; otherwise LLVM uses `{ tag, exact target-aligned carrier }`. On the
supported little-endian native targets, payload insertion and extraction pack
and unpack that carrier with SSA integer and aggregate operations at target-data
field offsets. Live carriers remain register values through calls, phis, and
loops: emission introduces no stack scratch, `memcpy`, universal value, GC, or
executor surface. The carrier layout is compiler-private and is not an FFI ABI.

An infallible function with no inout parameters returns its source result `T`
directly. With ordered functional writebacks `W...`, it returns `{ T, W... }`.
A faulting function returns `{ i32 status, T, W... }` and receives one hidden
fault-context pointer. Normal and fault exits both return the latest inout
values; the source result is zero-filled on a fault. This is a
compiler-private object ABI, not a native library ABI.

The production automatic route uses this typed ABI for eligible build, run,
and test artifacts. Tuple construction and `let` destructuring are direct SSA
construction and extraction; they do not allocate tuple nodes. Invariant-free
record projections and eligible projected mutable receivers use exact typed
extraction and functional root reconstruction on normal and fault edges.
Shapes outside the current typed-LCIR SupportReport—including non-regular
generic expansion, protected projections, unsupported contract/cleanup forms,
explicit mutable coroutine parameters, and finite/open dynamic coroutine
carriers—still select the complete universal route. Typed LCIR does not change
the checked-MIR runtime ABI or make either object ABI public.

See [Code generation IR](codegen-ir.md) for the implemented foundation and the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md) for the accepted
representation and migration design.

## Typed Tasks and coroutine frames

On the currently pinned 64-bit typed-task ABI, the direct representation of
every admitted `Task[T]` is one opaque scheduler-owned pointer (`TaskHandle`).
The exact `T` remains part of the LCIR semantic type and determines result
storage and take operations, but it adds no payload words, source-visible tag,
or managed-heap reference to the handle. Task handles are stable for their
scheduler lifetime and are not moving-GC roots.

A typed coroutine frame is target-laid out from its checked plan. It contains
state, parameters, optional creation-site span coordinates for async
preconditions, one ordered child-handle row and exact live-value row per
`AwaitTasks` suspension, and the exact completed result. Each plan row records
the output type of every awaited child before the forwarded live types. The
descriptor lists managed-leaf offsets only for values live in each state and
for the completed result; opaque child handles are scheduler bookkeeping, not
GC pointer cells.

A concrete closed `List[T]` or compiler-private `TextMap[V]` contributes one
managed-pointer cell at every frame occurrence, including nested product/sum
leaves. The collection's element pointer map remains in its typed repeated
descriptor. Frame validation accepts only canonical direct List and
`ManagedTextMap` registrations; a finite/open dynamic box cannot be relabeled as
a collection carrier.

An immediately awaited fixed tuple or fixed Task-policy call uses that
multi-child suspension row directly. A first-class stored fixed join uses a
separate exact composite frame containing state, ordered child handles, and its
target-laid-out result. `TaskJoin` still produces one opaque handle; its mode,
child-output row, and result type form the generated callback shape, and
identical shapes may share the immutable descriptor and callback. `Task.any`
also includes its producer origin in that key for exact fault blame. `all` and
`settled` preserve heterogeneous tuple rows, while `any` and `race` require one
homogeneous output type. A one-child fixed `all` or `settled` retains a
one-field tuple result rather than collapsing its type. No universal
`ValueSlot` or runtime-described join result participates in either path.
Outcome-producing rows and callbacks consume terminal affine child handles
through explicit `TaskOutcomeTake` operations. The resulting canonical sums
use the ordinary collision-free closed-sum carrier and exact managed Text
leaves for `TaskFault`. A sole nonempty List literal is flattened to the same
static row; empty, stored, computed, and runtime-sized List joins remain
complete fallback.

The runtime separately tracks concrete typed File and Socket owners held by a
published typed result; that ledger is not a field in `Task[T]` or in the
source value. Source records carry only their monotonic capability token. A
successful exact child-result take, including the Completed
branch of outcome take, moves the child's ledger entries to its owner Task,
which may itself be the root Task, before retiring the child. If result-take is
applied directly to the ownerless root Task, its entries remain attached to
that Task in the executor-owned task registry. Faulted, cancelled, losing, and
unconsumed tasks do not transfer entries. Terminal cleanup and typed result
disposal release their remaining typed resource owners at the deterministic
cleanup boundary, even if a disposer reports a fault or protocol defect;
retired-task reaping only reclaims memory. Validation failure commits neither a
topology change nor an ownership move.

Child extraction also rechecks its complete scheduler protocol before copying:
one exact owned/join membership, a settled successful join, result take only
for ALL/ANY, outcome take only for SETTLED/RACE, and completed winner
finalization for ANY/RACE. A mismatch leaves output cells, Task topology, and
the resource ledger unchanged.

## `Text`

A universal `Text` slot contains its tag and one pointer to a `TextObject`.
The object has a versioned layout descriptor, allocation size, UTF-8 byte
length, Unicode scalar length, and trailing UTF-8 bytes. Dynamically created
text is moved by the GC.

Typed LCIR uses one opaque pointer with that object shape. In a literal-only
artifact, `ImmortalText` points at compiler-emitted immutable globals with
process lifetime. If concat/get or a Text-bearing product/sum is reachable,
representation planning selects `ManagedPointer` for every Text in the
artifact, including literals. Dynamic
results are typed moving-GC leaves; literals remain legal static values in the
same pointer ABI. The callable graph never mixes those two LCIR
representations.

Length reads the scalar-count field. Containment and content equality use the
existing allocation-free containment helper, with equality also requiring
equal UTF-8 byte lengths. Source equality never compares object pointers. The
allocation-free Text operations themselves need no universal `ValueSlot` or
executor. `concat` and `get` are infallible LCIR safepoints with `MAY_COLLECT`, which
implies a synchronous runtime but no executor or source fault edge. Its
specialized helper stages both inputs before collection, allocates a typed leaf
with no pointer fields, initializes it without a safepoint, and publishes it
last. The get helper stages one selected Unicode scalar before its possible
allocation and maps missing indices to a nonallocating `None`. OOM is an
uncatchable process fault; malformed ABI status fails closed.

Every direct managed SSA value live after a collecting operation receives a
stable pointer cell in a typed shadow frame. A live unboxed product/sum expands
to deterministic candidate cells for its managed-pointer leaves; active tags
guard sum publication, definitions and phis publish the projections, and later
aggregate uses are rebuilt from possibly moved leaf reloads. Per-site bitmaps
are exact and results are excluded at their defining safepoint. Functions with
no live-across managed leaf emit no frame. Other dynamic producers, Text inside
transparent/refined carriers, and other unsupported managed shapes remain
complete checked-MIR fallback. Concrete closed Lists instead use direct managed
pointers and typed repeated descriptors.

All payload-bearing tagged sums use a target-data-derived byte-class plan.
Recursive managed cells classify pointer-width ranges; scalar, aggregate, and
padding bytes are non-pointer. The bounded planner chooses the lowest aligned
offset for each variant where those two classes never overlap, while allowing
same-class reuse. The carrier is zeroed before the active payload is inserted,
and packing, matching, root rebuild, and repeated descriptors share the one
plan. Carrier storage is capped at 64 KiB; all sums in an artifact share a
65,536-byte-step placement budget, and all pack/unpack sites independently
share a 65,536-payload-byte emission budget. These bounds contain search and
bytewise LLVM expansion across the whole artifact rather than per sum. This
applies to every admitted closed sum rather than recognizing a source type
name.

The canonical recursive `Json` value, admitted through List/TextMap pointer
cycle breakers, consequently remains 24 bytes on supported 64-bit targets:
tag byte 0, scalar payload byte 8, and managed payload pointer byte 16.
`List[Json]` traces only byte 16; a `TextMap[Json]` entry traces its Text key at
byte 0 and the Json pointer cell at byte 24. The layout exposes no stable
address, universal value, or runtime type tag, and unsupported or over-budget
layouts fail closed.

The descriptor is runtime trace/layout metadata. It is not a source-visible
tag and does not make `Text` a dynamic type. Literal objects and typed moving
objects reuse the existing language-visible layout prefix. The concat helper
and generated typed-root calls use the current Text and typed-GC boundaries
listed in [Versioning and compatibility](../project/versioning.md).

## Typed process input

Native process input uses the private typed-process-v1 boundary; it never
constructs a universal `ValueSlot`. When an artifact reaches argument count or
selection, generated `main` copies `argv[1..]` into one immutable runtime
snapshot before creating or activating the Loom runtime. An environment-only
artifact does not initialize that snapshot.

The count operation returns an `i64`; `-1` means that the snapshot is absent
and all negative values fail closed. Argument selection accepts an `i64` index
and one stable direct-Text output cell, returning `0` on success and nonzero on
an ABI defect. Environment lookup accepts a direct Text name and output cell;
its closed status domain is `-1` invalid, `0` missing, and `1` found. Both
allocating operations clear output first, publish the new Text last, and use
the normal typed shadow-root protocol. Environment lookup copies the input name
before its allocation safepoint, so a name dead after the call need not be kept
as a spurious root.

## Dynamic concept values

A `dyn C` value carries data plus an already selected conformance witness.
Witness descriptors contain only prerequisite and live method-slot tables
needed by generated code. Descriptor identity is not source RTTI.

The compiler may represent a known dynamic value with fewer machine values or
fold a witness into static code when that is unobservable. The semantic
requirement is selected conformance and value behavior, not a permanently
fixed two-word fat pointer.

Typed LCIR first tries the strongest closed-world form. When one exact
concept-and-binding view has exactly one reachable closed nongeneric witness,
the view is represented by its concrete value alone and every used requirement
becomes a direct call. Products, sums, and `List[dyn C]` use the concrete
layout recursively, so raw and erased values share one canonical physical
type.

When the artifact instead proves a finite set of two or more such witnesses,
the view has a compiler-private single-managed-pointer representation. Each
candidate is allocated in its own exact box layout: an ordinal tag followed by
that candidate's concrete payload. Candidate order is deterministic checked
artifact data, not runtime type identity. Each allocation uses a distinct
precise fixed-object GC descriptor, and dispatch switches on the finite tag
before making an ordinary direct typed call. A record, sum, or List stores only
that pointer; no witness pointer, fat pointer, universal value, or runtime
registry is present.

Readonly copies may share an immutable published box because object identity
and address are not observable. A `mut self` dispatch calls the concrete method
through its ordinary inout ABI, then allocates and writes back a fresh exact box
on both normal and fault exits. The old box is never modified, so independently
copied dynamic values retain logical value semantics across moving collection.
Missing witnesses and open, generic, prerequisite-dependent, or otherwise
incomplete sets still select structured unsupported classification for the
whole artifact.

Loom does not support runtime conversion from an untyped universal value to
`dyn C` by searching every conformance. This keeps witness reachability
closed-world.

## Managed layout and GC metadata

Managed allocations have static layout metadata sufficient for precise
tracing. Synchronous native frames publish pointers to live universal slots
through a versioned shadow-stack descriptor and per-state bitmaps. A separate
typed shadow-stack descriptor uses the same state/bitmap shape but its entries
point to direct pointer cells; the collector never guesses which slot
representation is present. Checked-MIR coroutine descriptors continue to publish
live universal Task-frame slots and captured witnesses. Typed coroutine and
stored-join descriptors instead publish exact target byte offsets and state
bitmaps for their statically known managed leaves. A typed root cell has a
stable address for its complete linked interval, may not reside in either
moving heap, and contains only null, an exact typed allocation base, or a
compiler-proven process-lifetime static/immortal pointer.

Typed managed objects do not carry a universal tag. A
`LoomGcObjectDescriptor` defines the required fixed prefix, exact allocation
alignment, and sorted byte offsets of every pointer-sized managed-reference
cell in that prefix. Pointer-free trailing bytes may extend the allocation.
The runtime validates and copies this metadata into a side table before an
allocation can become visible. At a moving collection it follows only those
cells and rewrites them together with typed root cells. Null and unregistered
static or immortal pointers remain unchanged. Checked-MIR moving pointers, interior
pointers, and unregistered finite-lifetime pointers are not legal typed cell
contents. The typed allocator's output cell likewise has a stable non-heap
address throughout its call and any collection that call triggers.

`LoomGcRepeatedObjectDescriptor` extends this model for monomorphized
containers. It describes a fixed header and one repeated element region with a
constant stride and exact managed-pointer offsets. Capacity is an allocator
argument copied into private GC metadata; tracing never trusts an object
header. The allocator scans zeroed unused capacity as null cells, keeping the
representation precise and tagless. `typed-repeated-v1` identifies this
compiler-private boundary.

The `loom_runtime_text_get_typed_v1` helper returns found, missing,
or invalid status and publishes a newly allocated one-scalar direct Text only
for found indices. It copies the scalar before collection, so source relocation
cannot invalidate the read.

The `loom_runtime_format_float_typed_v1(value, out_cell)` helper
publishes canonical binary64 text through the same direct managed pointer
representation. Its output cell remains stable across allocation and is
published only after complete initialization. `format-float-v1` identifies the
boundary.

The `loom_typed_timer_task_create_v1(executor, deadline_ns)` helper
returns the established opaque typed Task handle. Its `Task[Unit]` frame has no
managed roots and its result has zero size. `typed-timer-v1` identifies this
boundary.

`loom_typed_task_publish_adopting_v1(executor, composite, children, count)`
helper atomically transfers a nonempty ordered set of published typed children
from the active structured parent into one initialized unpublished composite,
then publishes that composite. It validates all pointers and ownership edges
and finishes fallible reservations before the first topology mutation; an error
therefore preserves the original ownership and ready-queue state.
`typed-task-adopt-v1` identifies the boundary.

Direct fixed `Task.any` reuses the same opaque handles, frames, and typed result
take. The existing join-step entry additionally finalizes completed loser
results and retires losers before returning the observable step. This adds no
source-value layout. Winner finalization is part of the current private Task
protocol.

Static `Task.settled` and `Task.race` use
`loom_typed_task_take_outcome_v1` to move one exact completed result or publish
managed fault Text before retiring a terminal child. Generalized winner
finalization is shared by `any` and `race`. The current private identity names
`typed-task-winner-finalize-v1` and `typed-task-outcome-v1`; Task handles and
source `TaskOutcome[T]` layouts do not gain an extra runtime tag or pointer.

The exact-length native harness stdout boundary is identified by `stdout-v1`
and changes no source-value, Task, coroutine, wait, Text, or GC layout.

Witness descriptors emitted by the compiler are immutable process-lifetime
constants. Dynamically assembled witness instances live in a non-moving proof
arena because generated hidden arguments can retain their address across a
safepoint; the arena is still marked and swept.

## Specialized local storage

The checked-MIR LLVM route retains a narrow non-escaping local `List[Int]` layout
using contiguous `i64` storage with length and capacity. It applies only when a
closed-world use scan proves no copy, escape, generic/witness boundary,
suspension, or cleanup hazard. All other list shapes use the complete generic
representation.

Element access on this proved layout retains the allocation's pointer
provenance through typed LLVM GEP instructions. Exact range scans can therefore
be vectorized without weakening the source bounds rules. The append lowering
also carries its range induction value and non-observable length in SSA so the
private allocation cannot make LLVM conservatively reload or update the header
on every iteration. Length is synchronized at allocation-growth boundaries and
normal loop exit; an element expression that references its receiver keeps
eager length updates.

Similarly, checked integer and flat-record optimizations are private lowering
choices. They must never appear as requirements in the language reference.

## ABI change checklist

A native-layout change normally requires:

- a new shared ABI identity or component version;
- generated LLVM declaration and field-offset updates;
- runtime implementation and trace/clone/drop updates;
- runtime-bundle compatibility checks;
- native-object fingerprint invalidation;
- forced-moving-GC, malformed-descriptor, differential, and release tests.

Do not preserve an old layout merely to maintain accidental compatibility. If
external native interoperability is added, it needs a separately specified,
stable boundary with explicit conversion.
