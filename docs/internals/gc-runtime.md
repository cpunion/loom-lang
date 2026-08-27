# GC runtime

Loom uses automatic memory management without ownership or borrow syntax. The
native runtime implements a precise moving collector. Movement and allocation
addresses are not observable in the language.

## Runtime ownership

`LoomRuntime` owns:

- the managed heap;
- independent universal-value and typed-pointer shadow-stack root chains;
- collector counters and threshold state;
- at most one attached async executor.

Synchronous code can use the runtime without constructing an executor, OS
poller, ready queue, or worker mailbox. A runtime can be active on only one
thread at a time; nested activation is valid only for the same runtime.
Destruction fails while the runtime is active, has roots, or still has an
attached executor.

## Collection policy

The heap begins with a 64 KiB collection threshold. Allocation charge grows
with managed allocations. After collection, the next threshold is the larger
of 64 KiB and twice the exact live footprint.

Collection occurs at compiler-known safepoints, before an allocation slow path,
or between coroutine resume calls. The production collector is
threshold-driven; tests can force a collection at every poll or before every
managed allocation.

## Precise roots

Legacy synchronous generated frames link a shadow-stack record containing:

- a versioned immutable descriptor;
- pointers to existing universal value slots;
- one current state;
- a bitmap row selecting the slots live in that state.

The compiler publishes the state before a safepoint. Runtime helper operations
use temporary precise root scopes when they hold partially constructed values.

Typed synchronous frames use the same state/bitmap model on a separate chain.
Each live entry points to writable pointer-sized storage containing a direct
managed reference. It is never interpreted as a universal `ValueSlot`. A cell
may contain only null, the exact base of a typed managed allocation, or a
compiler-proven process-lifetime static/immortal pointer. Legacy moving object
pointers, interior pointers, and other unregistered finite-lifetime pointers
are forbidden. A legal static pointer is left unchanged without adding a
runtime tag or registration table.

The slot cells, slot-pointer array, descriptor, and frame have stable addresses
for the entire linked interval. In particular, slot cells cannot live inside
either moving heap. Generated code may update cell contents and the published
state, but collection must never invalidate the storage that the frame names.

Both descriptor forms are validated before a collection can increment its
counter, trace, sweep, or move any allocation. One descriptor is limited to
65,536 slots, 65,536 states, and 1,048,576 bitmap words in total across all
states. Each independent chain is limited to 65,536 linked frames. The limits
are shared ABI constants so compiler rejection and hostile-runtime-input
validation agree. Both LLVM emitters reject an oversized root-map shape as
`ProgramTooLarge` before emitting a descriptor; this is an emission error, not
unsupported source coverage and never a fallback signal.

Async values live across suspension are stored in Task slots. Coroutine
descriptors provide the exact live bitmap for each state. Task results remain
roots only while their ownership and result state require it.

Derived list indexes and other non-owning acceleration structures are not
roots and are cleared before relocation.

## Typed moving-object ABI

The typed heap is a side-by-side extension of the existing universal heap. A
typed allocation has no universal `Value` envelope and no required in-object
tag. `LoomGcObjectDescriptor` supplies a fixed pointer-bearing prefix size,
object alignment, and a strictly increasing list of exact pointer-cell byte
offsets. An allocation may append pointer-free trailing bytes. The allocator
copies the validated offsets into its private side table, zero-initializes the
complete allocation, and does not retain caller descriptor or offset pointers.
Every non-null fixed pointer cell follows the same exact-typed-base or
static/immortal restriction as a typed root.

The version-one symbols are:

- `loom_gc_typed_alloc_v1(descriptor, allocation_size, output)`;
- `loom_gc_typed_repeated_alloc_v1(descriptor, capacity, output)`;
- `loom_gc_typed_root_push_v1(frame)`;
- `loom_gc_typed_root_pop_v1(frame)`;
- the shared `loom_gc_safepoint_v1()`.

Allocation size is capped at 1 GiB, alignment at 4,096 bytes, and fixed pointer
cells at 4,096 per object. Pointer offsets must be naturally aligned, strictly
increasing, and wholly inside the fixed prefix. The allocation uses the exact
advertised alignment. Invalid metadata, ABI mismatch, and resource exhaustion
have distinct status codes; ordinary out-of-memory remains an uncatchable
process-level fault.

`output` is set to null before validation or an allocation safepoint. Its cell
address must remain stable throughout the complete call and any triggered
collection, and the cell cannot reside in either moving heap. After the runtime
owns the allocation and copied trace metadata, it publishes the zeroed base
address to `output`. A runtime helper may stage source data, allocate into a
stable private out-cell, initialize without another safepoint, and publish its
final language result last.

The typed Text helpers follow that publication rule. Concat stages both
complete UTF-8 inputs. `loom_runtime_text_get_typed_v1(source, index, output)`
stages the selected scalar in four bytes of native stack storage before its
allocation boundary; a negative or out-of-range index returns the missing
status without collecting. It never constructs a universal `Value`.

`LoomGcRepeatedObjectDescriptor` adds a fixed header followed by elements of a
constant stride. It carries exact pointer offsets for the header and for one
element. The repeated allocator derives size from its capacity argument,
copies both bounded tables, and stores capacity in private side metadata; the
collector never trusts a mutable object length or capacity field. The complete
allocation is zeroed and unused element cells stay null, so scanning capacity
is exact rather than conservative. Each table is capped at 4,096 entries and
one allocation may describe at most 16,777,216 pointer cells.

Concrete Lists and compiler-private typed TextMaps share this descriptor wire.
A TextMap object has a fixed length header and repeated `{ Text key, V value }`
entries. Its element table always contains the key pointer offset plus every
exact managed leaf offset in the concrete closed `V`; target data determines
all padding and stride. There is no map-specific universal value, runtime tag,
layout registry, or tracing callback. Nested Lists and TextMaps are ordinary
managed-pointer leaves, while product and sum values contribute their precise
projected cells.

Every payload-bearing tagged sum uses a collision-free byte-class carrier
plan. For each target-laid-out variant, pointer-width ranges beginning at its
recursive managed offsets are pointer bytes; scalar, aggregate, and padding
bytes are non-pointer. The compiler places pointer-free variants first, then
chooses the lowest aligned offset for every pointer-bearing variant where the
two classes never cross. Pointer bytes may overlap other pointer bytes, and
non-pointer bytes may overlap other non-pointer bytes. Construction zeroes the
complete carrier before writing the active payload. The collector can
therefore scan the union pointer table without consulting the tag and without
ever treating an inactive scalar bit pattern as an address.

The same target-data plan drives carrier packing, unpacking, root rebuild, and
repeated descriptors. The canonical recursive `Json` consequence remains a
24-byte value on 64-bit targets, with its managed cell at byte 16;
`List[Json]` has stride 24/pointer offset 16 and a `{ Text, Json }` TextMap
entry has stride 32/pointer offsets 0 and 24. A nested sum containing Json and
a three-Int tuple is 40 bytes with its managed cell at byte 32. This general
representation reuses `typed-repeated-v1` and the typed shadow stack. It adds
no universal `Value`, object tag, tracing callback, executor, or runtime ABI
symbol. Bounded planning and checked arithmetic fail closed before an unsafe
descriptor can be emitted.

## Moving collection

The collector traces live universal values, managed nodes and sequences, text
objects, typed objects, Task frames, and witness instances. It builds
replacement allocations, updates every precise root and internal managed
pointer, then releases dead old storage. Typed tracing follows only copied
fixed or repeated pointer offsets. Parent-to-child graphs and cycles are marked
without recursion; relocation rewrites both typed root cells and typed object
fields.

Runtime clone and aggregate-building helpers use an explicit non-recursive work
stack. This avoids host stack overflow on deeply nested but valid managed
values and makes partially built graphs visible to the root protocol.

Compiler-emitted witness descriptors are static and do not move. Runtime-owned
witness instances use a separate non-moving arena because generated proof
arguments can hold their addresses across a safepoint; unreachable instances
are marked and swept.

Typed LCIR uses two artifact-wide Text modes. A literal-only, product-free
artifact keeps its immutable compiler-emitted `TextObject` globals outside the
managed heap, so their process-lifetime pointers need no relocation. If any
reachable function uses concat/get or places Text in a tuple/record/closed sum,
every Text has the direct managed-capable pointer representation.
`loom_runtime_text_concat_typed_v1(left, right, output)` copies and validates
both complete input byte sequences before its typed allocation can collect,
then creates one pointer-free typed leaf with a 32-byte header and trailing
UTF-8 bytes. It initializes without another safepoint and publishes the output
last. This staging rule also makes aliased inputs safe when collection moves
the old object.
`loom_runtime_text_get_typed_v1(text, scalar_index, output)` similarly stages
one selected Unicode scalar before allocating its direct Text leaf. Missing
indices do not allocate.

LCIR functions publish exact live-after typed-root bitmap states at concat/get and
at calls whose transitive effects may collect. A direct Text value uses one
stable pointer cell. A live unboxed product/sum expands to stable candidate
cells for its deterministically projected Text leaves; active sum tags guard
publication, definitions and block parameters update those cells, and
post-safepoint aggregate uses are rebuilt from reloaded leaves.
An edge argument is live only when the paired explicit successor parameter is
live, and a call result cannot be live at its own safepoint. No live-across
managed leaves means no typed frame. Synchronous concat/get and concrete closed
List or TextMap allocation use a runtime but construct no executor. A
collecting List site roots and reloads its old backing and managed element. A
collecting TextMap insertion similarly roots and reloads its old backing, Text
key, and exact managed leaves of its value before copying into fresh functional
storage. Functional removal consumes the search key before its allocation
boundary, roots and reloads exactly the source backing, and copies the surviving
typed entries into new storage; missing and last-entry removals do not allocate.
Repeated pointer offsets precisely cover every used or zeroed capacity cell.
Text inside transparent/refined carriers and other dynamic producers remain
outside the current typed LCIR slice.

## Source semantics

GC must not change:

- value equality or independent copy behavior;
- contract and refined-value checks;
- concept selection or dynamic dispatch;
- lexical `scoped` and `defer` cleanup ordering.

External resources such as files and sockets are not released by finalizers.
They use explicit close operations or lexical cleanup. The current core has no
source finalizer, weak-reference, stable-address, or general pinning API.

An out-of-memory condition is a process-level runtime fault rather than a
recoverable value. A future FFI boundary must copy data or define an explicit
bounded pin protocol; ordinary Loom values do not become immovable.

## Evidence

Runtime unit tests cover activation, attachment, both root descriptor forms,
shared resource bounds, copied typed layout metadata, advertised alignment,
forced collection, pointer-free trailing-byte preservation, typed
parent/child graphs, cycles, aliased and state-selective roots, legacy/typed
coexistence, relocation, nested managed values, witness mark/sweep, and
partial-construction helpers. The synchronous typed tests also prove concat
staging across forced relocation and that the heap path constructs no
executor. LLVM integration tests exercise exact Text live-after maps, alias
reloads, no-empty-frame emission, synchronous shadow-stack maps, coroutine
state liveness, structured values, standard-library outputs, and collections
at compiler-generated boundaries.

A complete Windows native job is configured with GC/runtime fixture coverage,
but it is not verified Windows runtime or release evidence until a real Windows
runner result is recorded.
